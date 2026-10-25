//! russh client handler: host key verification, banner, disconnect signal.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use russh::client::{self, DisconnectReason};
use russh::keys::{Algorithm, HashAlg, PublicKey, PublicKeyOrCertificate};
use tokio_util::sync::CancellationToken;

use super::SessionCtx;
use super::route::Hop;
use crate::engine::PromptResponse;
use crate::error::{Error, HostKeyError, Result};
use crate::events::{PromptKind, SessionEvent, SessionPhase};
use crate::known_hosts::{self, HostKeyVerdict, KnownHosts};
use crate::logs::LogLevel;
use crate::secrets::SecretKey;
use crate::ssh_config::StrictHostKeyChecking;

/// Everything needed to run the handshake for one hop.
pub(crate) struct Built {
    pub(crate) handler: ClientHandler,
    pub(crate) config: Arc<client::Config>,
    /// Precise host key failure, read when `connect_stream` fails.
    pub(crate) failure: Arc<Mutex<Option<HostKeyError>>>,
    /// Cancelled when the transport is closed by the peer or the network.
    pub(crate) closed: CancellationToken,
    /// Identity of the server once its key was verified.
    pub(crate) identity: Arc<Mutex<Option<HostIdentity>>>,
}

/// The verified server identity: what remembered passwords are bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostIdentity {
    /// `known_hosts` name (`HostKeyAlias` or lowercased `HostName`).
    pub(crate) name: String,
    pub(crate) port: u16,
    /// SHA256 fingerprint of the accepted host key (or certified key).
    pub(crate) fingerprint: String,
    /// How the key came to be trusted.
    pub(crate) trust: HostTrust,
}

/// How a host key was trusted for this connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostTrust {
    /// A `known_hosts` line matched.
    KnownHostsMatch,
    /// A host certificate signed by a `@cert-authority` of `known_hosts`.
    Certificate,
    /// Unknown key, accepted by the user (Trust On First Use prompt).
    UserAccepted,
    /// Unknown key, accepted without asking
    /// (`StrictHostKeyChecking accept-new` or `no`): no user gesture vouches
    /// for this server, so a password never bound to a key is withheld.
    AutoAccepted,
}

/// The name and port `resolved` is looked up under in `known_hosts`: the
/// `HostKeyAlias` (port 22: OpenSSH ignores the port for it) or the
/// lowercased `HostName` and port. Remembered passwords are bound to it.
pub(crate) fn known_hosts_name(resolved: &crate::ssh_config::ResolvedHost) -> (String, u16) {
    match resolved.options.get("hostkeyalias").and_then(|v| v.first()) {
        // OpenSSH takes a single alias and ignores the port for it.
        Some((args, _)) if !args.is_empty() => (
            args.first()
                .map(|a| a.to_ascii_lowercase())
                .unwrap_or_default(),
            22,
        ),
        // known_hosts names are lowercase (OpenSSH lowercases before hashing).
        _ => (resolved.hostname.to_ascii_lowercase(), resolved.port),
    }
}

/// Where and how to verify the server key.
#[derive(Debug)]
struct HostCheck {
    /// Name matched in `known_hosts` (`HostKeyAlias` or `HostName`).
    name: String,
    port: u16,
    known: KnownHosts,
    write_file: PathBuf,
    policy: StrictHostKeyChecking,
    /// `HashKnownHosts yes`: record new keys with hashed names.
    hash_names: bool,
    /// The user logging in (remembered passwords are looked up for it).
    user: String,
}

pub(crate) struct ClientHandler {
    ctx: SessionCtx,
    check: HostCheck,
    failure: Arc<Mutex<Option<HostKeyError>>>,
    closed: CancellationToken,
    identity: Arc<Mutex<Option<HostIdentity>>>,
    /// How the presented key was trusted, set by [`Self::verify`].
    trust: Option<HostTrust>,
    /// Only the final hop may deliver `-R` connections (a jump host never
    /// received a `tcpip-forward` request from us).
    is_target: bool,
}

/// The `known_hosts` files for `hop`: user files (the first one receives
/// new keys), then global files (kept apart: on Windows they are only
/// trusted when the machine owns them, see
/// [`known_hosts::trusted_global_files`]).
fn known_hosts_files(
    paths: &crate::store::Paths,
    hop: &Hop,
) -> (Vec<PathBuf>, Vec<PathBuf>, PathBuf) {
    let mut user_files = hop.resolved.user_known_hosts_files.clone();
    if user_files.is_empty() {
        user_files = vec![paths.known_hosts(), paths.ssh_dir.join("known_hosts2")];
    }
    let global_files: Vec<PathBuf> = hop
        .resolved
        .options
        .get("globalknownhostsfile")
        .and_then(|v| v.first())
        .map_or_else(
            || paths.global_known_hosts.clone(),
            |(args, _)| {
                args.iter()
                    .map(|a| crate::ssh_config::expand_home(a, &paths.home))
                    .collect()
            },
        );
    let write_file = user_files
        .first()
        .cloned()
        .unwrap_or_else(|| paths.known_hosts());
    (user_files, global_files, write_file)
}

pub(crate) async fn build(ctx: &SessionCtx, hop: &Hop, is_target: bool) -> Result<Built> {
    let paths = ctx.engine.config().paths.clone();
    let (mut files, global_files, write_file) = known_hosts_files(&paths, hop);
    let (known, skipped) = tokio::task::spawn_blocking(move || {
        let (trusted, skipped) = known_hosts::trusted_global_files(global_files);
        files.extend(trusted);
        KnownHosts::load(&files).map(|known| (known, skipped))
    })
    .await
    .map_err(|_| Error::EngineClosed)??;
    for w in skipped.iter().chain(&known.warnings) {
        ctx.log(LogLevel::Warn, "known_hosts", w.clone());
    }

    let (name, port) = known_hosts_name(&hop.resolved);
    known_hosts::validate_host_name(&name)?;

    let mut preferred = russh::Preferred::default();
    // Never negotiate SHA-1 `ssh-rsa` host keys (OpenSSH ≥ 8.8 default).
    preferred.key = preferred
        .key
        .iter()
        .filter(|a| !matches!(a, Algorithm::Rsa { hash: None }))
        .cloned()
        .collect::<Vec<_>>()
        .into();
    let known_algorithms = known.known_algorithms(&name, port);
    if !known_algorithms.is_empty() {
        // Prefer an algorithm we already have a key for (avoids spurious
        // "unknown key" prompts when a server has several host keys).
        let mut keys = preferred.key.to_vec();
        keys.sort_by_key(|a| !known_algorithms.iter().any(|k| same_family(k, a)));
        preferred.key = keys.into();
    }
    if known.has_cert_authority(&name, port) {
        preferred.host_key_certificates.clone_from(&preferred.key);
        ctx.log(
            LogLevel::Debug,
            "known_hosts",
            "host certificates accepted (@cert-authority)",
        );
    }
    let config = Arc::new(client::Config {
        preferred,
        nodelay: true,
        ..client::Config::default()
    });

    let failure = Arc::new(Mutex::new(None));
    let closed = CancellationToken::new();
    let identity = Arc::new(Mutex::new(None));
    Ok(Built {
        handler: ClientHandler {
            ctx: ctx.clone(),
            check: HostCheck {
                name,
                port,
                known,
                write_file,
                policy: hop.resolved.strict_host_key_checking,
                hash_names: hop
                    .resolved
                    .options
                    .get("hashknownhosts")
                    .and_then(|v| v.first())
                    .and_then(|(args, _)| args.first())
                    .is_some_and(|v| v.eq_ignore_ascii_case("yes")),
                user: hop.user.clone(),
            },
            failure: Arc::clone(&failure),
            closed: closed.clone(),
            identity: Arc::clone(&identity),
            trust: None,
            is_target,
        },
        config,
        failure,
        closed,
        identity,
    })
}

fn same_family(a: &Algorithm, b: &Algorithm) -> bool {
    matches!((a, b), (Algorithm::Rsa { .. }, Algorithm::Rsa { .. })) || a == b
}

impl ClientHandler {
    fn fail(&self, error: HostKeyError) -> bool {
        self.ctx.log(LogLevel::Error, "hostkey", error.to_string());
        *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
        false
    }

    async fn verify(&mut self, presented: &PublicKeyOrCertificate) -> Result<bool> {
        let (key, verdict) = match presented {
            PublicKeyOrCertificate::PublicKey { key, .. } => (
                key.clone(),
                self.check
                    .known
                    .check_key(&self.check.name, self.check.port, key),
            ),
            PublicKeyOrCertificate::Certificate(cert) => {
                let now =
                    u64::try_from(time::OffsetDateTime::now_utc().unix_timestamp()).unwrap_or(0);
                (
                    PublicKey::from(cert.public_key().clone()),
                    self.check.known.check_certificate(
                        &self.check.name,
                        self.check.port,
                        cert,
                        now,
                    ),
                )
            }
        };
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        let algorithm = key.algorithm().to_string();
        let host = known_hosts::host_field(&self.check.name, self.check.port);

        match verdict {
            HostKeyVerdict::Trusted { file, line } => {
                self.ctx.log(
                    LogLevel::Info,
                    "hostkey",
                    format!(
                        "{algorithm} {fingerprint} matches {}:{line}",
                        file.display()
                    ),
                );
                self.trust = Some(HostTrust::KnownHostsMatch);
                Ok(true)
            }
            HostKeyVerdict::TrustedCertificate { ca_fingerprint } => {
                self.ctx.log(
                    LogLevel::Info,
                    "hostkey",
                    format!("host certificate signed by trusted CA {ca_fingerprint}"),
                );
                self.trust = Some(HostTrust::Certificate);
                Ok(true)
            }
            HostKeyVerdict::Revoked { file, line } => {
                Ok(self.fail(HostKeyError::Revoked { host, file, line }))
            }
            HostKeyVerdict::InvalidCertificate { reason } => {
                Ok(self.fail(HostKeyError::InvalidCertificate { host, reason }))
            }
            HostKeyVerdict::Changed {
                file,
                line,
                expected_fingerprint,
            } => {
                self.ctx.event(SessionEvent::HostKeyChanged {
                    host: self.check.name.clone(),
                    port: self.check.port,
                    algorithm,
                    fingerprint: fingerprint.clone(),
                    expected_fingerprint: expected_fingerprint.clone(),
                    known_hosts_file: file.display().to_string(),
                    known_hosts_line: line,
                });
                Ok(self.fail(HostKeyError::Changed {
                    host,
                    actual: fingerprint,
                    expected: expected_fingerprint,
                    file,
                    line,
                }))
            }
            HostKeyVerdict::Unknown { other_algorithms } => {
                self.on_unknown(&key, host, algorithm, fingerprint, other_algorithms)
                    .await
            }
        }
    }

    /// Applies `StrictHostKeyChecking` to a key absent from `known_hosts`.
    async fn on_unknown(
        &mut self,
        key: &PublicKey,
        host: String,
        algorithm: String,
        fingerprint: String,
        other_algorithms: Vec<String>,
    ) -> Result<bool> {
        match self.check.policy {
            StrictHostKeyChecking::Yes => {
                return Ok(self.fail(HostKeyError::UnknownStrict { host }));
            }
            // A known host suddenly offering another key type is a classic
            // downgrade/MITM pattern: never accept it silently.
            StrictHostKeyChecking::AcceptNew | StrictHostKeyChecking::No
                if !other_algorithms.is_empty() =>
            {
                return Ok(self.fail(HostKeyError::NewAlgorithm {
                    host,
                    known: other_algorithms.join(", "),
                }));
            }
            StrictHostKeyChecking::AcceptNew | StrictHostKeyChecking::No => {
                self.ctx.log(
                    LogLevel::Warn,
                    "hostkey",
                    format!(
                        "accepting new host key {algorithm} {fingerprint} (StrictHostKeyChecking)"
                    ),
                );
                self.trust = Some(HostTrust::AutoAccepted);
            }
            StrictHostKeyChecking::Ask => {
                let sends_remembered_password = self.has_remembered_password(&fingerprint).await;
                let answer = self
                    .ctx
                    .ask(PromptKind::UnknownHostKey {
                        host: self.check.name.clone(),
                        port: self.check.port,
                        algorithm: algorithm.clone(),
                        fingerprint: fingerprint.clone(),
                        other_algorithms,
                        sends_remembered_password,
                    })
                    .await?;
                if !matches!(answer, PromptResponse::AcceptHostKey) {
                    return Ok(self.fail(HostKeyError::Rejected { host }));
                }
                self.ctx.log(
                    LogLevel::Info,
                    "hostkey",
                    format!("user trusted {algorithm} {fingerprint}"),
                );
                self.trust = Some(HostTrust::UserAccepted);
            }
        }
        self.remember(key).await;
        Ok(true)
    }

    /// `true` when the secret store holds a password the session would send
    /// once this key is accepted: one bound to this key, or one typed when
    /// the host was created and never bound yet. Blocking lookup off the
    /// runtime; a store error counts as "none".
    async fn has_remembered_password(&self, fingerprint: &str) -> bool {
        let store = Arc::clone(&self.ctx.engine.config().secrets);
        let bound = SecretKey::HostPassword {
            user: self.check.user.clone(),
            host: self.check.name.clone(),
            port: self.check.port,
            host_key: fingerprint.to_owned(),
        };
        let unbound = SecretKey::UnboundHostPassword {
            user: self.check.user.clone(),
            host: self.check.name.clone(),
            port: self.check.port,
        };
        tokio::task::spawn_blocking(move || {
            [bound, unbound]
                .iter()
                .any(|key| matches!(store.get(key), Ok(Some(_))))
        })
        .await
        .unwrap_or(false)
    }

    /// Appends the key to `known_hosts`. A write failure is logged, not fatal
    /// (OpenSSH behaves the same): the user already accepted the key.
    async fn remember(&self, key: &PublicKey) {
        let (file, name, port, key, hashed) = (
            self.check.write_file.clone(),
            self.check.name.clone(),
            self.check.port,
            key.clone(),
            self.check.hash_names,
        );
        let written = tokio::task::spawn_blocking(move || {
            known_hosts::append(&file, &name, port, &key, hashed).map(|()| file)
        })
        .await;
        match written {
            Ok(Ok(file)) => self.ctx.log(
                LogLevel::Info,
                "hostkey",
                format!(
                    "added {} to {}",
                    known_hosts::host_field(&self.check.name, self.check.port),
                    file.display()
                ),
            ),
            Ok(Err(e)) => self.ctx.log(
                LogLevel::Warn,
                "hostkey",
                format!("could not record host key: {e}"),
            ),
            Err(_) => self
                .ctx
                .log(LogLevel::Warn, "hostkey", "could not record host key"),
        }
    }
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    // russh 0.64 calls this for the initial key exchange only; re-keys are
    // not re-verified (see docs/STACK.md decision log).
    async fn check_server_key(
        &mut self,
        presented: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        self.ctx.phase(SessionPhase::VerifyingHostKey);
        match self.verify(presented).await {
            Ok(true) => {
                let key = match presented {
                    PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
                    PublicKeyOrCertificate::Certificate(cert) => {
                        PublicKey::from(cert.public_key().clone())
                    }
                };
                *self.identity.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(HostIdentity {
                        name: self.check.name.clone(),
                        port: self.check.port,
                        fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
                        // Never unset after `Ok(true)`; the safest reading otherwise.
                        trust: self.trust.unwrap_or(HostTrust::AutoAccepted),
                    });
                Ok(true)
            }
            Ok(false) => Ok(false),
            Err(e) => {
                // Cancelled while prompting: refuse the key, the actor reports why.
                self.ctx.log(LogLevel::Info, "hostkey", e.to_string());
                Ok(false)
            }
        }
    }

    // ── Channels opened by the server ────────────────────────────────────
    // russh accepts them all by default. Portico refuses everything except
    // the `-R` connections it asked for: no agent or X11 forwarding, no
    // server-initiated sessions or direct-tcpip.

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<client::Msg>,
        connected_address: &str,
        connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        if !self.is_target {
            self.ctx.log(
                LogLevel::Warn,
                "tunnel",
                "a jump host tried to open a forwarded connection: refused",
            );
            reply
                .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return Ok(());
        }
        crate::forward::serve_forwarded(
            &self.ctx,
            &self.ctx.routes,
            channel,
            reply,
            (connected_address, connected_port),
            (originator_address, originator_port),
        );
        Ok(())
    }

    async fn server_channel_open_forwarded_streamlocal(
        &mut self,
        _channel: russh::Channel<client::Msg>,
        _socket_path: &str,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        refuse(&self.ctx, "forwarded-streamlocal", reply).await;
        Ok(())
    }

    async fn server_channel_open_agent_forward(
        &mut self,
        _channel: russh::Channel<client::Msg>,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        refuse(&self.ctx, "agent forwarding", reply).await;
        Ok(())
    }

    async fn server_channel_open_session(
        &mut self,
        _channel: russh::Channel<client::Msg>,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        refuse(&self.ctx, "session", reply).await;
        Ok(())
    }

    async fn server_channel_open_direct_tcpip(
        &mut self,
        _channel: russh::Channel<client::Msg>,
        _host_to_connect: &str,
        _port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        refuse(&self.ctx, "direct-tcpip", reply).await;
        Ok(())
    }

    async fn server_channel_open_direct_streamlocal(
        &mut self,
        _channel: russh::Channel<client::Msg>,
        _socket_path: &str,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        refuse(&self.ctx, "direct-streamlocal", reply).await;
        Ok(())
    }

    async fn server_channel_open_x11(
        &mut self,
        _channel: russh::Channel<client::Msg>,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        refuse(&self.ctx, "X11", reply).await;
        Ok(())
    }

    async fn auth_banner(
        &mut self,
        banner: &str,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let text = banner.trim_end().to_owned();
        if !text.is_empty() {
            self.ctx.log(LogLevel::Info, "banner", text.clone());
            self.ctx.event(SessionEvent::Banner { text });
        }
        Ok(())
    }

    async fn disconnected(
        &mut self,
        reason: DisconnectReason<Self::Error>,
    ) -> Result<(), Self::Error> {
        self.closed.cancel();
        match reason {
            DisconnectReason::ReceivedDisconnect(info) => {
                if !info.message.is_empty() {
                    self.ctx.log(
                        LogLevel::Info,
                        "session",
                        format!("server said: {}", info.message),
                    );
                }
                Ok(())
            }
            DisconnectReason::Error(e) => Err(e),
        }
    }
}

async fn refuse(ctx: &SessionCtx, what: &str, reply: client::ChannelOpenHandle) {
    ctx.log(
        LogLevel::Warn,
        "session",
        format!("the server tried to open a {what} channel: refused"),
    );
    reply
        .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
        .await;
}
