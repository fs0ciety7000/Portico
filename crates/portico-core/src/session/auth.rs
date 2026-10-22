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
//! The secret store holds passwords bound to the verified host key
//! ([`SecretKey::HostPassword`]). A password typed when a host was created in
//! Portico has no key yet ([`SecretKey::UnboundHostPassword`]): it is offered
//! only after the host key passed verification, when no bound one exists, and
//! the first login it opens binds it to that key (moved, never copied). It is
//! withheld when the key was accepted with no user gesture
//! (`StrictHostKeyChecking accept-new|no`, [`HostTrust::AutoAccepted`]), it
//! is forgotten when the server rejects it, and after a login another typed
//! password opened; a rejected one is never bound (keyboard-interactive
//! tracks whether the last password answer came from the store). Every
//! binding is recorded in the non-secret index of `hosts.toml`
//! ([`crate::hosts::record_bound_password`]) so "forget" finds it.
//!
//! Secrets never reach logs: only fingerprints, paths and method names do.

use std::path::{Path, PathBuf};

use russh::client::{AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::{Certificate, HashAlg, PublicKey};
use russh::{MethodKind, MethodSet};
use secrecy::{ExposeSecret, SecretString};

use super::handler::{ClientHandler, HostIdentity, HostTrust};
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

/// A configured identity and what we know about it without decrypting it.
#[derive(Debug)]
struct FileIdentity {
    path: PathBuf,
    public: PublicKey,
    cert: Option<Certificate>,
    /// `false` for public-key-only identities (`IdentityFile k.pub`): they
    /// only select which agent key to use.
    has_private: bool,
}

/// Methods Portico drives, in OpenSSH's default order.
const DEFAULT_ORDER: [&str; 3] = ["publickey", "keyboard-interactive", "password"];

#[derive(Debug)]
enum Attempt<'a> {
    AgentKey(PublicKey),
    AgentCert(Certificate),
    FileKey(&'a FileIdentity),
    FileCert(&'a FileIdentity, Certificate),
}

impl Attempt<'_> {
    fn is_rsa(&self) -> bool {
        let alg = match self {
            Self::AgentKey(k) => k.algorithm(),
            Self::FileKey(f) => f.public.algorithm(),
            Self::AgentCert(c) | Self::FileCert(_, c) => c.algorithm(),
        };
        matches!(alg, russh::keys::Algorithm::Rsa { .. })
    }

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

/// The answers to one keyboard-interactive info request.
struct InfoAnswer {
    answers: Vec<String>,
    /// The secret to remember (the user asked for it).
    remember: Option<SecretString>,
    /// Answered from the secret store (the remembered password).
    from_store: bool,
}

struct Auth<'a> {
    ctx: &'a SessionCtx,
    hop: &'a Hop,
    identity: &'a HostIdentity,
    handle: &'a mut Handle<ClientHandler>,
    methods: MethodSet,
    tried: Vec<&'static str>,
    password_attempts: u32,
}

pub(crate) async fn authenticate(
    ctx: &SessionCtx,
    handle: &mut Handle<ClientHandler>,
    hop: &Hop,
    identity: &HostIdentity,
) -> Result<AuthMethod> {
    let methods = match handle
        .authenticate_none(hop.user.clone())
        .await
        .map_err(|e| transport(hop, &e))?
    {
        // Accepted without any key: proves nothing about one.
        AuthResult::Success if hop.pubkey_only => {
            return Err(Error::AuthFailed {
                user: hop.user.clone(),
                host: hop.alias.clone(),
                tried: "none (the server needs no key)".into(),
            });
        }
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

    if methods.is_empty() && handle.is_closed() {
        return Err(Error::AuthDisconnected {
            host: hop.alias.clone(),
        });
    }

    let mut auth = Auth {
        ctx,
        hop,
        identity,
        handle,
        methods,
        tried: Vec::new(),
        password_attempts: hop
            .option_u32("numberofpasswordprompts")
            .unwrap_or(PASSWORD_ATTEMPTS),
    };
    for method in method_order(hop) {
        let done = match method {
            "publickey" => auth.publickey().await?,
            "keyboard-interactive" => auth
                .keyboard_interactive()
                .await?
                .then_some(AuthMethod::KeyboardInteractive),
            _ => auth.password().await?.then_some(AuthMethod::Password),
        };
        if let Some(m) = done {
            return Ok(m);
        }
        if auth.handle.is_closed() {
            return Err(Error::AuthDisconnected {
                host: hop.alias.clone(),
            });
        }
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

/// `PreferredAuthentications` order minus methods disabled by
/// `PubkeyAuthentication` / `KbdInteractiveAuthentication` /
/// `PasswordAuthentication no`.
fn method_order(hop: &Hop) -> Vec<&'static str> {
    let preferred: Vec<&'static str> = match hop.option_str("preferredauthentications") {
        Some(list) => list
            .split(',')
            .filter_map(|m| {
                DEFAULT_ORDER
                    .iter()
                    .copied()
                    .find(|d| d.eq_ignore_ascii_case(m.trim()))
            })
            .collect(),
        None => DEFAULT_ORDER.to_vec(),
    };
    // Key login proof: publickey whatever the config prefers (like
    // `ssh -o PreferredAuthentications=publickey`).
    if hop.pubkey_only {
        return vec!["publickey"];
    }
    let disabled = |key: &str| {
        hop.option_str(key)
            .is_some_and(|v| v.eq_ignore_ascii_case("no"))
    };
    preferred
        .into_iter()
        .filter(|m| match *m {
            "publickey" => !disabled("pubkeyauthentication"),
            "keyboard-interactive" => {
                !disabled("kbdinteractiveauthentication")
                    && !disabled("challengeresponseauthentication")
            }
            _ => !disabled("passwordauthentication"),
        })
        .collect()
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
        let mut attempts = plan_attempts(&identities, &agent_ids, identities_only);
        if self.hop.pubkey_only {
            // Key login proof: a certificate would succeed without the key
            // being in authorized_keys.
            attempts.retain(|a| !matches!(a, Attempt::AgentCert(_) | Attempt::FileCert(..)));
        }
        if attempts.is_empty() {
            self.ctx
                .log(LogLevel::Info, "auth", "no public key identities available");
            return Ok(None);
        }
        // `None`: the server did not advertise rsa-sha2-*; SHA-1 `ssh-rsa`
        // stays disabled (OpenSSH ≥ 8.8 default), so RSA keys are skipped.
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
            if attempt.is_rsa() && rsa_hash.is_none() {
                self.ctx.log(
                    LogLevel::Info,
                    "auth",
                    format!("{label}: skipped (server does not advertise rsa-sha2-*; ssh-rsa/SHA-1 disabled)"),
                );
                continue;
            }
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
                Err(e @ (SignError::Send(_) | SignError::Key(_))) => {
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
                let mut signer = KeySigner::agent(a, self.ctx);
                let r = self
                    .handle
                    .authenticate_publickey_with(user, key, hash, &mut signer)
                    .await;
                self.note_outcome(&signer, label);
                r
            }
            (Attempt::AgentCert(cert), Some(a)) => {
                let hash = cert_hash(&cert, rsa_hash);
                let mut signer = KeySigner::agent(a, self.ctx);
                let r = self
                    .handle
                    .authenticate_certificate_with(user, cert, hash, &mut signer)
                    .await;
                self.note_outcome(&signer, label);
                r
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
        match signer.outcome() {
            Outcome::NotRequested => {
                self.ctx.log(
                    LogLevel::Debug,
                    "auth",
                    format!("{label}: not accepted by the server"),
                );
            }
            Outcome::Skipped => {
                self.ctx
                    .log(LogLevel::Info, "auth", format!("{label}: skipped"));
            }
            Outcome::Signed => {}
        }
    }

    async fn connect_agent(&self) -> Option<Agent> {
        let socket = match self.hop.resolved.identity_agent.as_deref() {
            Some(v) if v.eq_ignore_ascii_case("none") => return None,
            // Already `~`/`${VAR}`/token-expanded by the config resolver.
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
        let (mut stored, unbound) = match self.stored_password().await {
            Some((secret, unbound)) => (Some(secret), unbound),
            None => (None, false),
        };
        for _ in 0..self.password_attempts {
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
            // The remembered password answered a password prompt of this
            // exchange and was not rejected since.
            let mut store_answer_pending = false;
            // The user typed the answer to a password prompt.
            let mut typed_password = false;
            for _ in 0..MAX_INFO_ROUNDS {
                match response {
                    KeyboardInteractiveAuthResponse::Success => {
                        if let Some(secret) = remember {
                            self.save_password(secret).await;
                        }
                        if store_answer_pending {
                            if unbound {
                                self.bind_unbound_password().await;
                            } else {
                                self.index_bound_password().await;
                            }
                        } else if typed_password {
                            self.forget_unbound_after_other_secret().await;
                        }
                        return Ok(true);
                    }
                    KeyboardInteractiveAuthResponse::Failure {
                        remaining_methods, ..
                    } => {
                        if !remaining_methods.is_empty() {
                            self.methods = remaining_methods;
                        }
                        if store_answer_pending && unbound {
                            self.forget_rejected_unbound_password().await;
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
                        let password_prompt = is_password_prompt(&prompts);
                        if store_answer_pending && password_prompt {
                            // Asked for the password again: the remembered
                            // one was rejected. It is never bound now, even
                            // if the user then types the right one.
                            store_answer_pending = false;
                            if unbound {
                                self.forget_rejected_unbound_password().await;
                            } else {
                                self.ctx.log(
                                    LogLevel::Warn,
                                    "auth",
                                    "remembered password was rejected",
                                );
                            }
                        }
                        let Some(answer) = self
                            .answer_info_request(name, instructions, &prompts, &mut stored)
                            .await?
                        else {
                            self.abandon_keyboard_interactive(prompts.len()).await?;
                            return Ok(false);
                        };
                        if answer.from_store {
                            store_answer_pending = true;
                        } else if password_prompt {
                            typed_password = true;
                        }
                        remember = answer.remember;
                        response = self
                            .handle
                            .authenticate_keyboard_interactive_respond(answer.answers)
                            .await
                            .map_err(|e| transport(self.hop, &e))?;
                    }
                }
            }
        }
        Ok(false)
    }

    /// The user dismissed a prompt: answer this and any further info request
    /// with empty strings until the server concludes, so russh stays in sync
    /// (it would otherwise swallow the next auth request).
    async fn abandon_keyboard_interactive(&mut self, first_prompts: usize) -> Result<()> {
        let mut count = first_prompts;
        for _ in 0..MAX_INFO_ROUNDS {
            let response = self
                .handle
                .authenticate_keyboard_interactive_respond(vec![String::new(); count])
                .await
                .map_err(|e| transport(self.hop, &e))?;
            match response {
                KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                    count = prompts.len();
                }
                KeyboardInteractiveAuthResponse::Failure {
                    remaining_methods, ..
                } => {
                    if !remaining_methods.is_empty() {
                        self.methods = remaining_methods;
                    }
                    return Ok(());
                }
                KeyboardInteractiveAuthResponse::Success => return Ok(()),
            }
        }
        Err(Error::Transport {
            target: self.hop.alias.clone(),
            message: "keyboard-interactive did not conclude".to_owned(),
        })
    }

    /// Answers one info request: from the secret store for a lone password
    /// prompt (once), otherwise by asking the user. `None` if the user
    /// dismissed the prompt.
    async fn answer_info_request(
        &self,
        name: String,
        instructions: String,
        prompts: &[russh::client::Prompt],
        stored: &mut Option<SecretString>,
    ) -> Result<Option<InfoAnswer>> {
        if prompts.is_empty() {
            return Ok(Some(InfoAnswer {
                answers: Vec::new(),
                remember: None,
                from_store: false,
            }));
        }
        if is_password_prompt(prompts)
            && let Some(pw) = stored.take()
        {
            self.ctx.log(
                LogLevel::Info,
                "auth",
                "answering password prompt from the secret store",
            );
            return Ok(Some(InfoAnswer {
                answers: vec![pw.expose_secret().to_owned()],
                remember: None,
                from_store: true,
            }));
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
            // sshd drops the connection when the answer count is wrong.
            PromptResponse::Answers(values) if values.len() == prompts.len() => Some(InfoAnswer {
                answers: values
                    .iter()
                    .map(|v| v.expose_secret().to_owned())
                    .collect(),
                remember: None,
                from_store: false,
            }),
            PromptResponse::Secret { value, remember } if prompts.len() == 1 => {
                let answers = vec![value.expose_secret().to_owned()];
                Some(InfoAnswer {
                    answers,
                    remember: remember.then_some(value),
                    from_store: false,
                })
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
        if let Some((stored, unbound)) = self.stored_password().await {
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
            drop(stored);
            if self.settle(r) {
                if unbound {
                    self.bind_unbound_password().await;
                } else {
                    // Bound before the index existed: indexed now.
                    self.index_bound_password().await;
                }
                return Ok(true);
            }
            if unbound {
                self.forget_rejected_unbound_password().await;
            } else {
                self.ctx
                    .log(LogLevel::Warn, "auth", "remembered password was rejected");
            }
        }
        for attempt in 1..=self.password_attempts {
            if !self.allows(MethodKind::Password) {
                return Ok(false);
            }
            let answer = self
                .ctx
                .ask(PromptKind::Password {
                    user: self.hop.user.clone(),
                    host: self.hop.alias.clone(),
                    attempt,
                    attempts: self.password_attempts,
                })
                .await?;
            let PromptResponse::Secret { value, remember } = answer else {
                // Dismissing the last-resort prompt means "stop", not "failed".
                self.ctx
                    .log(LogLevel::Info, "auth", "password prompt dismissed");
                return Err(Error::Cancelled);
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
                self.forget_unbound_after_other_secret().await;
                return Ok(true);
            }
            self.ctx.log(
                LogLevel::Warn,
                "auth",
                format!("password rejected ({attempt}/{})", self.password_attempts),
            );
        }
        Ok(false)
    }

    fn password_key(&self) -> SecretKey {
        SecretKey::HostPassword {
            user: self.hop.user.clone(),
            host: self.identity.name.clone(),
            port: self.identity.port,
            host_key: self.identity.fingerprint.clone(),
        }
    }

    fn unbound_password_key(&self) -> SecretKey {
        SecretKey::UnboundHostPassword {
            user: self.hop.user.clone(),
            host: self.identity.name.clone(),
            port: self.identity.port,
        }
    }

    /// `true` when the host key was accepted with no user gesture
    /// (`StrictHostKeyChecking accept-new|no`): a password never bound to a
    /// key is then withheld.
    fn auto_accepted(&self) -> bool {
        self.identity.trust == HostTrust::AutoAccepted
    }

    /// The remembered password: the one bound to this host key, else the
    /// one typed when the host was created in Portico (`true`: unbound),
    /// unless the key was accepted automatically.
    async fn stored_password(&self) -> Option<(SecretString, bool)> {
        let store = std::sync::Arc::clone(&self.ctx.engine.config().secrets);
        let bound = self.password_key();
        let unbound = self.unbound_password_key();
        let found = tokio::task::spawn_blocking(move || {
            if let Ok(Some(secret)) = store.get(&bound) {
                return Some((secret, false));
            }
            store
                .get(&unbound)
                .ok()
                .flatten()
                .map(|secret| (secret, true))
        })
        .await
        .ok()
        .flatten();
        match found {
            Some((_, true)) if self.auto_accepted() => {
                self.ctx.log(
                    LogLevel::Warn,
                    "auth",
                    "remembered password withheld: new host key accepted automatically",
                );
                None
            }
            other => other,
        }
    }

    /// Forgets the unbound password: the server rejected it.
    async fn forget_rejected_unbound_password(&self) {
        if self.delete_unbound_password().await {
            self.ctx.log(
                LogLevel::Warn,
                "auth",
                "remembered password was rejected and forgotten",
            );
        }
    }

    /// Forgets the unbound password after a login that another secret (a
    /// password the user typed) opened: it was not the right one, or no
    /// longer needed. Kept when it was withheld (never tried).
    async fn forget_unbound_after_other_secret(&self) {
        if self.auto_accepted() {
            return;
        }
        if self.delete_unbound_password().await {
            self.ctx.log(
                LogLevel::Info,
                "secrets",
                "remembered password forgotten: another password opened the session",
            );
        }
    }

    /// Deletes the unbound password; `true` when one was deleted.
    async fn delete_unbound_password(&self) -> bool {
        let store = std::sync::Arc::clone(&self.ctx.engine.config().secrets);
        let unbound = self.unbound_password_key();
        let deleted = tokio::task::spawn_blocking(move || -> crate::error::Result<bool> {
            if store.get(&unbound)?.is_none() {
                return Ok(false);
            }
            store.delete(&unbound)?;
            Ok(true)
        })
        .await;
        match deleted {
            Ok(Ok(deleted)) => deleted,
            // The error code only: never a secret.
            Ok(Err(e)) => {
                self.ctx.log(
                    LogLevel::Warn,
                    "secrets",
                    format!("could not forget the password ({})", e.summary().code),
                );
                false
            }
            Err(_) => false,
        }
    }

    /// Records, in the non-secret index of `hosts.toml`, that the password
    /// of this account is bound to this host key (so "forget" finds it
    /// whatever `known_hosts` says later). Blocking work off the runtime.
    async fn index_bound_password(&self) {
        let path = self.ctx.engine.config().paths.hosts_meta();
        let (user, host, port, fingerprint) = (
            self.hop.user.clone(),
            self.identity.name.clone(),
            self.identity.port,
            self.identity.fingerprint.clone(),
        );
        let recorded = tokio::task::spawn_blocking(move || {
            crate::hosts::record_bound_password(&path, &user, &host, port, &fingerprint)
        })
        .await;
        if let Ok(Err(e)) = recorded {
            self.ctx.log(
                LogLevel::Warn,
                "secrets",
                format!(
                    "could not index the remembered password ({})",
                    e.summary().code
                ),
            );
        }
    }

    /// Moves the unbound password to the key of this (verified) server:
    /// from now on only this server identity receives it.
    async fn bind_unbound_password(&self) {
        let store = std::sync::Arc::clone(&self.ctx.engine.config().secrets);
        let bound = self.password_key();
        let unbound = self.unbound_password_key();
        let moved = tokio::task::spawn_blocking(move || -> crate::error::Result<bool> {
            let Some(secret) = store.get(&unbound)? else {
                return Ok(false);
            };
            store.set(&bound, &secret)?;
            store.delete(&unbound)?;
            Ok(true)
        })
        .await;
        if matches!(moved, Ok(Ok(true))) {
            self.index_bound_password().await;
        }
        match moved {
            Ok(Ok(true)) => self.ctx.log(
                LogLevel::Info,
                "secrets",
                "password bound to this server's host key",
            ),
            Ok(Ok(false)) => {}
            // The error code only: never a secret.
            Ok(Err(e)) => self.ctx.log(
                LogLevel::Warn,
                "secrets",
                format!(
                    "could not bind the password to the host key ({})",
                    e.summary().code
                ),
            ),
            Err(_) => self.ctx.log(
                LogLevel::Warn,
                "secrets",
                "could not bind the password to the host key",
            ),
        }
    }

    async fn save_password(&self, secret: SecretString) {
        let store = std::sync::Arc::clone(&self.ctx.engine.config().secrets);
        let key = self.password_key();
        let saved = tokio::task::spawn_blocking(move || store.set(&key, &secret)).await;
        match saved {
            Ok(Ok(())) => {
                self.ctx.log(
                    LogLevel::Info,
                    "secrets",
                    "password remembered in the secret store",
                );
                self.index_bound_password().await;
            }
            // The error code only (`vault_locked`, `secrets`…): never a secret.
            Ok(Err(e)) => self.ctx.log(
                LogLevel::Warn,
                "secrets",
                format!("could not remember the password ({})", e.summary().code),
            ),
            Err(_) => self
                .ctx
                .log(LogLevel::Warn, "secrets", "could not remember the password"),
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
    // Certificate held by the agent for this key: signed there, no passphrase.
    let agent_cert = |key: &PublicKey| {
        agent.iter().find_map(|id| match id {
            AgentIdentity::Certificate { certificate, .. }
                if certificate.public_key() == key.key_data() =>
            {
                Some(certificate.clone())
            }
            _ => None,
        })
    };
    let mut planned_certs: Vec<Certificate> = Vec::new();
    let mut attempts = Vec::new();
    // a. configured identities held by the agent
    for f in files.iter().filter(|f| agent_has(&f.public)) {
        if let Some(cert) = agent_cert(&f.public) {
            planned_certs.push(cert.clone());
            attempts.push(Attempt::AgentCert(cert));
        } else if let Some(cert) = f.cert.as_ref().filter(|_| f.has_private) {
            attempts.push(Attempt::FileCert(f, cert.clone()));
        }
        attempts.push(Attempt::AgentKey(f.public.clone()));
    }
    // b. other agent identities
    if !identities_only {
        for id in agent {
            match id {
                AgentIdentity::Certificate { certificate, .. } => {
                    if !planned_certs.contains(certificate) {
                        attempts.push(Attempt::AgentCert(certificate.clone()));
                    }
                }
                AgentIdentity::PublicKey { key, .. } => {
                    if !files.iter().any(|f| f.public.key_data() == key.key_data()) {
                        attempts.push(Attempt::AgentKey(key.clone()));
                    }
                }
            }
        }
    }
    // c. configured identities not in the agent (needs the private file)
    for f in files
        .iter()
        .filter(|f| f.has_private && !agent_has(&f.public))
    {
        if let Some(cert) = &f.cert {
            attempts.push(Attempt::FileCert(f, cert.clone()));
        }
        attempts.push(Attempt::FileKey(f));
    }
    attempts
}

/// Reads public halves (and certificates) without decrypting anything.
/// Accepts `IdentityFile k`, `IdentityFile k.pub`, and a missing `k` whose
/// `k.pub` exists (agent-only identity), like OpenSSH.
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
    for configured in paths {
        let (private, public) = match locate(configured) {
            Located::Private(p) => {
                let Some(public) = public_half(&p) else {
                    notes.push(format!(
                        "cannot read the public key of {} (encrypted legacy key without .pub?)",
                        p.display()
                    ));
                    continue;
                };
                (Some(p), public)
            }
            Located::PublicOnly(pub_path) => {
                let Some(public) = read_public(&pub_path) else {
                    notes.push(format!("invalid public key {}", pub_path.display()));
                    continue;
                };
                let private = PathBuf::from(pub_path.to_string_lossy().trim_end_matches(".pub"));
                (private.is_file().then_some(private), public)
            }
            Located::Missing => {
                if explicit {
                    notes.push(format!("identity {} not found", configured.display()));
                }
                continue;
            }
        };
        let base = private.clone().unwrap_or_else(|| configured.clone());
        let base = PathBuf::from(base.to_string_lossy().trim_end_matches(".pub"));
        let cert = certs
            .iter()
            .find(|c| c.public_key() == public.key_data())
            .cloned()
            .or_else(|| {
                let sidecar = PathBuf::from(format!("{}-cert.pub", base.display()));
                std::fs::read_to_string(sidecar)
                    .ok()
                    .and_then(|t| Certificate::from_openssh(t.trim()).ok())
                    .filter(|c| c.public_key() == public.key_data())
            });
        identities.push(FileIdentity {
            has_private: private.is_some(),
            path: private.unwrap_or(base),
            public,
            cert,
        });
    }
    (identities, notes)
}

enum Located {
    Private(PathBuf),
    PublicOnly(PathBuf),
    Missing,
}

fn locate(path: &Path) -> Located {
    let is_pub = path.extension().is_some_and(|e| e == "pub");
    if is_pub && path.is_file() {
        return Located::PublicOnly(path.to_path_buf());
    }
    if path.is_file() {
        return Located::Private(path.to_path_buf());
    }
    let sidecar = PathBuf::from(format!("{}.pub", path.display()));
    if sidecar.is_file() {
        return Located::PublicOnly(sidecar);
    }
    Located::Missing
}

fn read_public(path: &Path) -> Option<PublicKey> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| PublicKey::from_openssh(t.trim()).ok())
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
