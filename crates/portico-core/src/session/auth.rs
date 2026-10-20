//! Client authentication, in OpenSSH order:
//!
//! 1. `none` (learns the allowed methods);
//! 2. `publickey`, with each identity's certificate first:
//!    a. configured identities that the agent holds (agent signs),
//!    b. other agent identities (unless `IdentitiesOnly`),
//!    c. configured identities not in the agent (file signer, lazy unlock);
//! 3. `keyboard-interactive` (a lone hidden "password" prompt may be
//!    answered from the secret store);
//! 4. `password` (secret store first, then up to 3 prompts).
//!
//! Secrets never reach logs: only fingerprints, paths and method names do.

use std::path::{Path, PathBuf};

use russh::client::{AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::{Certificate, HashAlg, PublicKey};
use russh::{MethodKind, MethodSet};
use secrecy::{ExposeSecret, SecretString};

use super::handler::ClientHandler;
use super::route::Hop;
use super::signer::{Agent, FileSigner, KeySigner, Outcome, SignError};
use super::{SessionCtx, transport};
use crate::engine::PromptResponse;
use crate::error::{Error, Result};
use crate::events::{AuthMethod, InteractivePrompt, PromptKind, SessionPhase};
use crate::logs::LogLevel;
use crate::secrets::SecretKey;

/// Password / keyboard-interactive attempts (`NumberOfPasswordPrompts`).
const PASSWORD_ATTEMPTS: u32 = 3;
/// Guard against servers looping keyboard-interactive info requests.
const MAX_INFO_ROUNDS: u32 = 16;

/// A private key file and what we know about it without decrypting it.
#[derive(Debug)]
struct FileIdentity {
    path: PathBuf,
    public: PublicKey,
    cert: Option<Certificate>,
}

#[derive(Debug)]
enum Attempt<'a> {
    AgentKey(PublicKey),
    AgentCert(Certificate),
    FileKey(&'a FileIdentity),
    FileCert(&'a FileIdentity, Certificate),
}

impl Attempt<'_> {
    /// Phase method + log label (fingerprints and paths only, no secrets).
    fn describe(&self) -> (AuthMethod, String) {
        match self {
            Self::AgentKey(k) => (
                AuthMethod::Agent,
                format!("agent key {}", k.fingerprint(HashAlg::Sha256)),
            ),
            Self::AgentCert(c) => (
                AuthMethod::Certificate,
                format!("agent certificate {}", c.key_id()),
            ),
            Self::FileKey(f) => (AuthMethod::PublicKey, format!("key {}", f.path.display())),
            Self::FileCert(f, c) => (
                AuthMethod::Certificate,
                format!("certificate {} ({})", c.key_id(), f.path.display()),
            ),
        }
    }
}

struct Auth<'a> {
    ctx: &'a SessionCtx,
    hop: &'a Hop,
    handle: &'a mut Handle<ClientHandler>,
    methods: MethodSet,
    tried: Vec<&'static str>,
}

pub(crate) async fn authenticate(
    ctx: &SessionCtx,
    handle: &mut Handle<ClientHandler>,
    hop: &Hop,
) -> Result<AuthMethod> {
    let methods = match handle
        .authenticate_none(hop.user.clone())
        .await
        .map_err(|e| transport(hop, &e))?
    {
        AuthResult::Success => return Ok(AuthMethod::None),
        AuthResult::Failure {
            remaining_methods, ..
        } => remaining_methods,
    };
    ctx.log(
        LogLevel::Debug,
        "auth",
        format!(
            "server allows: {}",
            methods
                .iter()
                .map(<&str>::from)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );

    let mut auth = Auth {
        ctx,
        hop,
        handle,
        methods,
        tried: Vec::new(),
    };
    if let Some(method) = auth.publickey().await? {
        return Ok(method);
    }
    if auth.keyboard_interactive().await? {
        return Ok(AuthMethod::KeyboardInteractive);
    }
    if auth.password().await? {
        return Ok(AuthMethod::Password);
    }
    Err(Error::AuthFailed {
        user: hop.user.clone(),
        host: hop.alias.clone(),
        tried: if auth.tried.is_empty() {
            "no usable method".to_owned()
        } else {
            auth.tried.join(", ")
        },
    })
}

impl Auth<'_> {
    fn allows(&self, kind: MethodKind) -> bool {
        self.methods.contains(&kind)
    }

    fn mark(&mut self, name: &'static str) {
        if !self.tried.contains(&name) {
            self.tried.push(name);
        }
    }

    /// Updates allowed methods after a failure. Returns `true` on success.
    fn settle(&mut self, result: AuthResult) -> bool {
        match result {
            AuthResult::Success => true,
            AuthResult::Failure {
                remaining_methods, ..
            } => {
                if !remaining_methods.is_empty() {
                    self.methods = remaining_methods;
                }
                false
            }
        }
    }

    // ── publickey ──────────────────────────────────────────────────────────

    async fn publickey(&mut self) -> Result<Option<AuthMethod>> {
        if !self.allows(MethodKind::PublicKey) {
            return Ok(None);
        }
        let identities = self.file_identities().await;
        let mut agent = self.connect_agent().await;
        let agent_ids: Vec<AgentIdentity> = match agent.as_mut() {
            Some(a) => match a.request_identities().await {
                Ok(ids) => ids,
                Err(e) => {
                    self.ctx.log(
                        LogLevel::Warn,
                        "agent",
                        format!("cannot list identities: {e}"),
                    );
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
        let identities_only = self.hop.resolved.identities_only;
        let attempts = plan_attempts(&identities, &agent_ids, identities_only);
        if attempts.is_empty() {
            self.ctx
                .log(LogLevel::Info, "auth", "no public key identities available");
            return Ok(None);
        }
        let rsa_hash = self
            .handle
            .best_supported_rsa_hash()
            .await
            .ok()
            .flatten()
            .flatten();

        for attempt in attempts {
            if !self.allows(MethodKind::PublicKey) {
                break;
            }
            let (method, label) = attempt.describe();
            self.ctx.phase(SessionPhase::Authenticating { method });
            self.ctx
                .log(LogLevel::Info, "auth", format!("trying {label}"));
            self.mark("publickey");
            let Some(result) = self
                .run_attempt(attempt, agent.as_mut(), rsa_hash, &label)
                .await
            else {
                continue;
            };

            match result {
                Ok(r) => {
                    if self.settle(r) {
                        return Ok(Some(method));
                    }
                    self.ctx
                        .log(LogLevel::Debug, "auth", format!("{label}: rejected"));
                }
                Err(SignError::Cancelled) => return Err(Error::Cancelled),
                Err(e) => {
                    // The transport may be out of sync after a signer failure.
                    return Err(Error::Transport {
                        target: self.hop.alias.clone(),
                        message: format!("{label}: {e}"),
                    });
                }
            }
        }
        Ok(None)
    }

    /// Runs one publickey attempt; `None` when it needs an absent agent.
    async fn run_attempt(
        &mut self,
        attempt: Attempt<'_>,
        agent: Option<&mut Agent>,
        rsa_hash: Option<HashAlg>,
        label: &str,
    ) -> Option<std::result::Result<AuthResult, SignError>> {
        let user = self.hop.user.clone();
        let result = match (attempt, agent) {
            (Attempt::AgentKey(key), Some(a)) => {
                let hash = hash_for(&key, rsa_hash);
                self.handle
                    .authenticate_publickey_with(user, key, hash, &mut KeySigner::Agent(a))
                    .await
            }
            (Attempt::AgentCert(cert), Some(a)) => {
                let hash = cert_hash(&cert, rsa_hash);
                self.handle
                    .authenticate_certificate_with(user, cert, hash, &mut KeySigner::Agent(a))
                    .await
            }
            (Attempt::AgentKey(_) | Attempt::AgentCert(_), None) => return None,
            (Attempt::FileKey(identity), _) => {
                let mut signer = KeySigner::File(file_signer(self.ctx, identity));
                let hash = hash_for(&identity.public, rsa_hash);
                let r = self
                    .handle
                    .authenticate_publickey_with(user, identity.public.clone(), hash, &mut signer)
                    .await;
                self.note_outcome(&signer, label);
                r
            }
            (Attempt::FileCert(identity, cert), _) => {
                let mut signer = KeySigner::File(file_signer(self.ctx, identity));
                let hash = cert_hash(&cert, rsa_hash);
                let r = self
                    .handle
                    .authenticate_certificate_with(user, cert, hash, &mut signer)
                    .await;
                self.note_outcome(&signer, label);
                r
            }
        };
        Some(result)
    }

    fn note_outcome(&self, signer: &KeySigner<'_>, label: &str) {
        if let KeySigner::File(file) = signer {
            match file.outcome {
                Outcome::NotRequested => {
                    self.ctx.log(
                        LogLevel::Debug,
                        "auth",
                        format!("{label}: not accepted by the server"),
                    );
                }
                Outcome::Skipped => {
                    self.ctx
                        .log(LogLevel::Info, "auth", format!("{label}: skipped (locked)"));
                }
                Outcome::Signed => {}
            }
        }
    }

    async fn connect_agent(&self) -> Option<Agent> {
        let socket = match self.hop.resolved.identity_agent.as_deref() {
            Some(v) if v.eq_ignore_ascii_case("none") => return None,
            Some(v) if v != "SSH_AUTH_SOCK" => Some(PathBuf::from(v)),
            _ => self.ctx.engine.config().agent_socket.clone(),
        }?;
        connect_agent_at(self.ctx, &socket).await
    }

    async fn file_identities(&self) -> Vec<FileIdentity> {
        let resolved = &self.hop.resolved;
        let explicit = !resolved.identity_files.is_empty();
        let paths = if explicit {
            resolved.identity_files.clone()
        } else {
            self.ctx.engine.config().paths.default_identities()
        };
        let cert_files = resolved.certificate_files.clone();
        let (loaded, notes) =
            tokio::task::spawn_blocking(move || load_identities(&paths, &cert_files, explicit))
                .await
                .unwrap_or_default();
        for note in notes {
            self.ctx.log(LogLevel::Info, "auth", note);
        }
        let now = u64::try_from(time::OffsetDateTime::now_utc().unix_timestamp()).unwrap_or(0);
        let mut out = Vec::with_capacity(loaded.len());
        for mut identity in loaded {
            if let Some(cert) = &identity.cert
                && (cert.valid_before() <= now || cert.valid_after() > now)
            {
                self.ctx.log(
                    LogLevel::Warn,
                    "auth",
                    format!(
                        "certificate {} for {} is not valid now: skipped",
                        cert.key_id(),
                        identity.path.display()
                    ),
                );
                identity.cert = None;
            }
            out.push(identity);
        }
        out
    }

    // ── keyboard-interactive ───────────────────────────────────────────────

    async fn keyboard_interactive(&mut self) -> Result<bool> {
        let mut stored = self.stored_password().await;
        for _ in 0..PASSWORD_ATTEMPTS {
            if !self.allows(MethodKind::KeyboardInteractive) {
                return Ok(false);
            }
            self.ctx.phase(SessionPhase::Authenticating {
                method: AuthMethod::KeyboardInteractive,
            });
            self.mark("keyboard-interactive");
            let mut response = self
                .handle
                .authenticate_keyboard_interactive_start(self.hop.user.clone(), None)
                .await
                .map_err(|e| transport(self.hop, &e))?;
            let mut remember: Option<SecretString> = None;
            let mut asked = false;
            for _ in 0..MAX_INFO_ROUNDS {
                match response {
                    KeyboardInteractiveAuthResponse::Success => {
                        if let Some(secret) = remember {
                            self.save_password(secret).await;
                        }
                        return Ok(true);
                    }
                    KeyboardInteractiveAuthResponse::Failure {
                        remaining_methods, ..
                    } => {
                        if !remaining_methods.is_empty() {
                            self.methods = remaining_methods;
                        }
                        if !asked {
                            // Rejected without a single question: retrying is pointless.
                            return Ok(false);
                        }
                        break;
                    }
                    KeyboardInteractiveAuthResponse::InfoRequest {
                        name,
                        instructions,
                        prompts,
                    } => {
                        asked |= !prompts.is_empty();
                        let Some((answers, keep)) = self
                            .answer_info_request(name, instructions, &prompts, &mut stored)
                            .await?
                        else {
                            // Keep the protocol in sync, then stop.
                            let empty = vec![String::new(); prompts.len()];
                            let _ = self
                                .handle
                                .authenticate_keyboard_interactive_respond(empty)
                                .await;
                            return Ok(false);
                        };
                        remember = keep;
                        response = self
                            .handle
                            .authenticate_keyboard_interactive_respond(answers)
                            .await
                            .map_err(|e| transport(self.hop, &e))?;
                    }
                }
            }
        }
        Ok(false)
    }

    /// Answers one info request: from the secret store for a lone password
    /// prompt, otherwise by asking the user. Returns the answers and the
    /// secret to remember (only when the user asked for it); `None` if the
    /// user dismissed the prompt.
    async fn answer_info_request(
        &self,
        name: String,
        instructions: String,
        prompts: &[russh::client::Prompt],
        stored: &mut Option<SecretString>,
    ) -> Result<Option<(Vec<String>, Option<SecretString>)>> {
        if prompts.is_empty() {
            return Ok(Some((Vec::new(), None)));
        }
        if is_password_prompt(prompts)
            && let Some(pw) = stored.take()
        {
            self.ctx.log(
                LogLevel::Info,
                "auth",
                "answering password prompt from the secret store",
            );
            return Ok(Some((vec![pw.expose_secret().to_owned()], None)));
        }
        let kind = PromptKind::KeyboardInteractive {
            name,
            instructions,
            prompts: prompts
                .iter()
                .map(|p| InteractivePrompt {
                    text: p.prompt.clone(),
                    echo: p.echo,
                })
                .collect(),
        };
        Ok(match self.ctx.ask(kind).await? {
            PromptResponse::Answers(values) => Some((
                values
                    .iter()
                    .map(|v| v.expose_secret().to_owned())
                    .collect(),
                None,
            )),
            PromptResponse::Secret { value, remember } => {
                let answer = vec![value.expose_secret().to_owned()];
                Some((answer, remember.then_some(value)))
            }
            _ => None,
        })
    }

    // ── password ───────────────────────────────────────────────────────────

    async fn password(&mut self) -> Result<bool> {
        if !self.allows(MethodKind::Password) {
            return Ok(false);
        }
        self.mark("password");
        if let Some(stored) = self.stored_password().await {
            self.ctx.phase(SessionPhase::Authenticating {
                method: AuthMethod::Password,
            });
            self.ctx
                .log(LogLevel::Info, "auth", "trying the remembered password");
            let r = self
                .handle
                .authenticate_password(self.hop.user.clone(), stored.expose_secret())
                .await
                .map_err(|e| transport(self.hop, &e))?;
            if self.settle(r) {
                return Ok(true);
            }
            self.ctx
                .log(LogLevel::Warn, "auth", "remembered password was rejected");
        }
        for attempt in 1..=PASSWORD_ATTEMPTS {
            if !self.allows(MethodKind::Password) {
                return Ok(false);
            }
            let answer = self
                .ctx
                .ask(PromptKind::Password {
                    user: self.hop.user.clone(),
                    host: self.hop.alias.clone(),
                })
                .await?;
            let PromptResponse::Secret { value, remember } = answer else {
                return Ok(false);
            };
            self.ctx.phase(SessionPhase::Authenticating {
                method: AuthMethod::Password,
            });
            let r = self
                .handle
                .authenticate_password(self.hop.user.clone(), value.expose_secret())
                .await
                .map_err(|e| transport(self.hop, &e))?;
            if self.settle(r) {
                if remember {
                    self.save_password(value).await;
                }
                return Ok(true);
            }
            self.ctx.log(
                LogLevel::Warn,
                "auth",
                format!("password rejected ({attempt}/{PASSWORD_ATTEMPTS})"),
            );
        }
        Ok(false)
    }

    fn password_key(&self) -> SecretKey {
        SecretKey::HostPassword {
            user: self.hop.user.clone(),
            host: self.hop.resolved.hostname.clone(),
            port: self.hop.resolved.port,
        }
    }

    async fn stored_password(&self) -> Option<SecretString> {
        let store = std::sync::Arc::clone(&self.ctx.engine.config().secrets);
        let key = self.password_key();
        tokio::task::spawn_blocking(move || store.get(&key))
            .await
            .ok()
            .and_then(std::result::Result::ok)
            .flatten()
    }

    async fn save_password(&self, secret: SecretString) {
        let store = std::sync::Arc::clone(&self.ctx.engine.config().secrets);
        let key = self.password_key();
        let saved = tokio::task::spawn_blocking(move || store.set(&key, &secret)).await;
        if matches!(saved, Ok(Ok(()))) {
            self.ctx.log(
                LogLevel::Info,
                "secrets",
                "password remembered in the secret store",
            );
        } else {
            self.ctx
                .log(LogLevel::Warn, "secrets", "could not remember the password");
        }
    }
}

fn file_signer<'a>(ctx: &'a SessionCtx, identity: &FileIdentity) -> FileSigner<'a> {
    FileSigner::new(
        ctx,
        identity.path.clone(),
        identity.public.fingerprint(HashAlg::Sha256).to_string(),
    )
}

fn is_password_prompt(prompts: &[russh::client::Prompt]) -> bool {
    matches!(prompts, [p] if !p.echo && p.prompt.to_ascii_lowercase().contains("password"))
}

fn hash_for(key: &PublicKey, rsa_hash: Option<HashAlg>) -> Option<HashAlg> {
    matches!(key.algorithm(), russh::keys::Algorithm::Rsa { .. })
        .then_some(rsa_hash)
        .flatten()
}

fn cert_hash(cert: &Certificate, rsa_hash: Option<HashAlg>) -> Option<HashAlg> {
    matches!(cert.algorithm(), russh::keys::Algorithm::Rsa { .. })
        .then_some(rsa_hash)
        .flatten()
}

fn plan_attempts<'a>(
    files: &'a [FileIdentity],
    agent: &[AgentIdentity],
    identities_only: bool,
) -> Vec<Attempt<'a>> {
    let agent_has = |key: &PublicKey| {
        agent.iter().any(|id| match id {
            AgentIdentity::PublicKey { key: k, .. } => k.key_data() == key.key_data(),
            AgentIdentity::Certificate { .. } => false,
        })
    };
    let mut attempts = Vec::new();
    // a. configured identities held by the agent
    for f in files.iter().filter(|f| agent_has(&f.public)) {
        if let Some(cert) = &f.cert {
            attempts.push(Attempt::FileCert(f, cert.clone()));
        }
        attempts.push(Attempt::AgentKey(f.public.clone()));
    }
    // b. other agent identities
    if !identities_only {
        for id in agent {
            match id {
                AgentIdentity::Certificate { certificate, .. } => {
                    attempts.push(Attempt::AgentCert(certificate.clone()));
                }
                AgentIdentity::PublicKey { key, .. } => {
                    if !files.iter().any(|f| f.public.key_data() == key.key_data()) {
                        attempts.push(Attempt::AgentKey(key.clone()));
                    }
                }
            }
        }
    }
    // c. configured identities not in the agent
    for f in files.iter().filter(|f| !agent_has(&f.public)) {
        if let Some(cert) = &f.cert {
            attempts.push(Attempt::FileCert(f, cert.clone()));
        }
        attempts.push(Attempt::FileKey(f));
    }
    attempts
}

/// Reads public halves (and certificates) without decrypting anything.
/// Returns the usable identities and diagnostic notes for the session log.
fn load_identities(
    paths: &[PathBuf],
    cert_files: &[PathBuf],
    explicit: bool,
) -> (Vec<FileIdentity>, Vec<String>) {
    let certs: Vec<Certificate> = cert_files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter_map(|t| Certificate::from_openssh(t.trim()).ok())
        .collect();
    let mut identities = Vec::new();
    let mut notes = Vec::new();
    for path in paths {
        if !path.is_file() {
            if explicit {
                notes.push(format!("identity {} not found", path.display()));
            }
            continue;
        }
        let Some(public) = public_half(path) else {
            notes.push(format!(
                "cannot read the public key of {} (encrypted legacy key without .pub?)",
                path.display()
            ));
            continue;
        };
        let cert = certs
            .iter()
            .find(|c| c.public_key() == public.key_data())
            .cloned()
            .or_else(|| {
                let sidecar = PathBuf::from(format!("{}-cert.pub", path.display()));
                std::fs::read_to_string(sidecar)
                    .ok()
                    .and_then(|t| Certificate::from_openssh(t.trim()).ok())
                    .filter(|c| c.public_key() == public.key_data())
            });
        identities.push(FileIdentity {
            path: path.clone(),
            public,
            cert,
        });
    }
    (identities, notes)
}

fn public_half(path: &Path) -> Option<PublicKey> {
    let sidecar = PathBuf::from(format!("{}.pub", path.display()));
    if let Some(key) = std::fs::read_to_string(sidecar)
        .ok()
        .and_then(|t| PublicKey::from_openssh(t.trim()).ok())
    {
        return Some(key);
    }
    let text = zeroize::Zeroizing::new(std::fs::read_to_string(path).ok()?);
    // OpenSSH-format keys keep the public half in clear, even when encrypted.
    if let Ok(key) = russh::keys::PrivateKey::from_openssh(text.as_bytes()) {
        return Some(key.public_key().clone());
    }
    russh::keys::decode_secret_key(&text, None)
        .ok()
        .map(|k| k.public_key().clone())
}

async fn connect_agent_at(ctx: &SessionCtx, socket: &Path) -> Option<Agent> {
    #[cfg(unix)]
    {
        match russh::keys::agent::client::AgentClient::connect_uds(socket).await {
            Ok(agent) => {
                ctx.log(
                    LogLevel::Debug,
                    "agent",
                    format!("connected to {}", socket.display()),
                );
                Some(agent.dynamic())
            }
            Err(e) => {
                ctx.log(
                    LogLevel::Warn,
                    "agent",
                    format!("cannot reach ssh-agent at {}: {e}", socket.display()),
                );
                None
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (ctx, socket);
        None
    }
}
