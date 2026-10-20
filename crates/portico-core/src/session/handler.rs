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
use crate::ssh_config::StrictHostKeyChecking;

/// Everything needed to run the handshake for one hop.
pub(crate) struct Built {
    pub(crate) handler: ClientHandler,
    pub(crate) config: Arc<client::Config>,
    /// Precise host key failure, read when `connect_stream` fails.
    pub(crate) failure: Arc<Mutex<Option<HostKeyError>>>,
    /// Cancelled when the transport is closed by the peer or the network.
    pub(crate) closed: CancellationToken,
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
}

pub(crate) struct ClientHandler {
    ctx: SessionCtx,
    check: HostCheck,
    failure: Arc<Mutex<Option<HostKeyError>>>,
    closed: CancellationToken,
    /// Key accepted during the first key exchange (re-keying presents it again).
    accepted: Option<Vec<u8>>,
}

/// Loads `known_hosts` for `hop` and prepares the russh configuration.
pub(crate) async fn build(ctx: &SessionCtx, hop: &Hop) -> Result<Built> {
    let paths = ctx.engine.config().paths.clone();
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
            |(args, _)| args.iter().map(PathBuf::from).collect(),
        );
    let write_file = user_files
        .first()
        .cloned()
        .unwrap_or_else(|| paths.known_hosts());
    let mut files = user_files;
    files.extend(global_files);

    let known = tokio::task::spawn_blocking(move || KnownHosts::load(&files))
        .await
        .map_err(|_| Error::EngineClosed)??;
    for w in &known.warnings {
        ctx.log(LogLevel::Warn, "known_hosts", w.clone());
    }

    let (name, port) = match hop
        .resolved
        .options
        .get("hostkeyalias")
        .and_then(|v| v.first())
    {
        Some((args, _)) if !args.is_empty() => (args.join(" "), 22),
        _ => (hop.resolved.hostname.clone(), hop.resolved.port),
    };

    let mut preferred = russh::Preferred::default();
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
    Ok(Built {
        handler: ClientHandler {
            ctx: ctx.clone(),
            check: HostCheck {
                name,
                port,
                known,
                write_file,
                policy: hop.resolved.strict_host_key_checking,
            },
            failure: Arc::clone(&failure),
            closed: closed.clone(),
            accepted: None,
        },
        config,
        failure,
        closed,
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
                Ok(true)
            }
            HostKeyVerdict::TrustedCertificate { ca_fingerprint } => {
                self.ctx.log(
                    LogLevel::Info,
                    "hostkey",
                    format!("host certificate signed by trusted CA {ca_fingerprint}"),
                );
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
            StrictHostKeyChecking::AcceptNew | StrictHostKeyChecking::No => {
                self.ctx.log(
                    LogLevel::Warn,
                    "hostkey",
                    format!(
                        "accepting new host key {algorithm} {fingerprint} (StrictHostKeyChecking)"
                    ),
                );
            }
            StrictHostKeyChecking::Ask => {
                let answer = self
                    .ctx
                    .ask(PromptKind::UnknownHostKey {
                        host: self.check.name.clone(),
                        port: self.check.port,
                        algorithm: algorithm.clone(),
                        fingerprint: fingerprint.clone(),
                        other_algorithms,
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
            }
        }
        self.remember(key).await;
        Ok(true)
    }

    /// Appends the key to `known_hosts`. A write failure is logged, not fatal
    /// (OpenSSH behaves the same): the user already accepted the key.
    async fn remember(&self, key: &PublicKey) {
        let (file, name, port, key) = (
            self.check.write_file.clone(),
            self.check.name.clone(),
            self.check.port,
            key.clone(),
        );
        let written = tokio::task::spawn_blocking(move || {
            known_hosts::append(&file, &name, port, &key).map(|()| file)
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

    async fn check_server_key(
        &mut self,
        presented: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let bytes = match presented {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.to_bytes().ok(),
            PublicKeyOrCertificate::Certificate(cert) => cert.to_bytes().ok(),
        };
        if bytes.is_some() && bytes == self.accepted {
            return Ok(true); // re-key: same key as accepted at connection time
        }
        self.ctx.phase(SessionPhase::VerifyingHostKey);
        match self.verify(presented).await {
            Ok(true) => {
                self.accepted = bytes;
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
