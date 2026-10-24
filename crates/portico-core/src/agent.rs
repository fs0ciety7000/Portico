//! Key management through `ssh-agent` (`ssh-add -l / -a / -d`) and the
//! engine entry points for local keys (list, generate).
//!
//! The agent socket is the engine's (`SSH_AUTH_SOCK`, or the configured
//! override). Private keys are decrypted in memory only to be handed to the
//! agent; the passphrase is never stored or logged. russh sends an empty
//! comment when adding an identity, so keys added here show no comment in
//! `ssh-add -l` (upstream limitation).

use std::path::{Path, PathBuf};
use std::time::Duration;

use russh::keys::HashAlg;
use russh::keys::agent::{AgentIdentity, Constraint};
use russh::keys::ssh_key::{PrivateKey, PublicKey};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::keys::{CertValidity, KeyError, KeyGenRequest, LocalKey, cert_validity};

/// Agent requests are local: anything slower means a stuck agent.
const AGENT_TIMEOUT: Duration = Duration::from_secs(10);

/// An identity held by the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentKey {
    pub algorithm: String,
    /// `SHA256:…` of the key (of the certified key for a certificate).
    pub fingerprint: String,
    pub comment: String,
    pub certificate: Option<AgentCertificate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCertificate {
    pub key_id: String,
    pub principals: Vec<String>,
    /// Unix seconds.
    pub valid_after: u64,
    /// Unix seconds (`u64::MAX` = forever).
    pub valid_before: u64,
}

impl AgentCertificate {
    pub fn validity_at(&self, unix_now: u64) -> CertValidity {
        cert_validity(self.valid_after, self.valid_before, unix_now)
    }
}

type Client = crate::session::signer::Agent;

impl Engine {
    /// Key pairs in `~/.ssh` (public halves only; never reads private keys).
    pub async fn local_keys(&self) -> Result<Vec<LocalKey>> {
        let dir = self.config().paths.ssh_dir.clone();
        tokio::task::spawn_blocking(move || crate::keys::discover(&dir))
            .await
            .map_err(|_| Error::EngineClosed)?
    }

    /// Generates a key pair in `~/.ssh` (see [`crate::keys::generate`]).
    pub async fn generate_key(&self, request: KeyGenRequest) -> Result<LocalKey> {
        let dir = self.config().paths.ssh_dir.clone();
        tokio::task::spawn_blocking(move || crate::keys::generate(&dir, &request))
            .await
            .map_err(|_| Error::EngineClosed)?
    }

    async fn agent(&self) -> Result<Client> {
        let socket = self.config().agent_socket.clone();
        let unavailable = || KeyError::AgentUnavailable {
            socket: socket.clone(),
        };
        let path = socket.clone().ok_or_else(unavailable)?;
        tokio::time::timeout(AGENT_TIMEOUT, crate::agent_socket::connect(&path))
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable().into())
    }

    /// `ssh-add -l`.
    pub async fn agent_keys(&self) -> Result<Vec<AgentKey>> {
        let mut agent = self.agent().await?;
        let identities = tokio::time::timeout(AGENT_TIMEOUT, agent.request_identities())
            .await
            .map_err(|_| KeyError::AgentRefused("no answer".into()))?
            .map_err(|e| KeyError::AgentRefused(e.to_string()))?;
        Ok(identities.into_iter().map(agent_key).collect())
    }

    /// `ssh-add [-t lifetime] <private_key>`. An encrypted key needs its
    /// passphrase ([`KeyError::NeedsPassphrase`] tells the UI to ask).
    pub async fn agent_add(
        &self,
        private_key: PathBuf,
        passphrase: Option<SecretString>,
        lifetime: Option<Duration>,
    ) -> Result<AgentKey> {
        let key =
            tokio::task::spawn_blocking(move || load_private(&private_key, passphrase.as_ref()))
                .await
                .map_err(|_| Error::EngineClosed)??;
        // `ssh-add -t 0` means no limit.
        let constraints: Vec<Constraint> = lifetime
            .filter(|l| !l.is_zero())
            .map(|l| Constraint::KeyLifetime {
                seconds: u32::try_from(l.as_secs()).unwrap_or(u32::MAX).max(1),
            })
            .into_iter()
            .collect();
        let mut agent = self.agent().await?;
        tokio::time::timeout(AGENT_TIMEOUT, agent.add_identity(&key, &constraints))
            .await
            .map_err(|_| KeyError::AgentRefused("no answer".into()))?
            .map_err(|e| KeyError::AgentRefused(e.to_string()))?;
        Ok(agent_key(AgentIdentity::from(key.public_key().clone())))
    }

    /// `ssh-add -d`, by fingerprint (as listed by [`Engine::agent_keys`]).
    /// The agent's answer is checked by listing again (russh does not
    /// report a refusal).
    pub async fn agent_remove(&self, fingerprint: &str) -> Result<()> {
        let mut agent = self.agent().await?;
        let identities = list(&mut agent).await?;
        let matching = |ids: &[AgentIdentity]| -> (Option<PublicKey>, bool) {
            let mut plain = None;
            let mut certificate = false;
            for id in ids
                .iter()
                .filter(|i| agent_key((*i).clone()).fingerprint == fingerprint)
            {
                match id {
                    AgentIdentity::PublicKey { key, .. } => plain = Some(key.clone()),
                    AgentIdentity::Certificate { .. } => certificate = true,
                }
            }
            (plain, certificate)
        };
        let (plain, certificate) = matching(&identities);
        let Some(public) = plain else {
            return Err(if certificate {
                // russh removes identities by plain key only: sending the
                // certified key would remove the wrong identity.
                KeyError::AgentRefused(
                    "certificates cannot be removed from Portico yet (use ssh-add -d)".into(),
                )
            } else {
                KeyError::NotInAgent {
                    fingerprint: fingerprint.to_owned(),
                }
            }
            .into());
        };
        tokio::time::timeout(AGENT_TIMEOUT, agent.remove_identity(&public))
            .await
            .map_err(|_| KeyError::AgentRefused("no answer".into()))?
            .map_err(|e| KeyError::AgentRefused(e.to_string()))?;
        let (still, certificate) = matching(&list(&mut agent).await?);
        if still.is_some() {
            return Err(
                KeyError::AgentRefused("the key is still loaded (agent locked?)".into()).into(),
            );
        }
        if certificate {
            // Same private key, still able to sign: say so.
            return Err(KeyError::AgentRefused(
                "the key was removed but its certificate is still loaded (use ssh-add -d)".into(),
            )
            .into());
        }
        Ok(())
    }
}

async fn list(agent: &mut Client) -> Result<Vec<AgentIdentity>> {
    tokio::time::timeout(AGENT_TIMEOUT, agent.request_identities())
        .await
        .map_err(|_| KeyError::AgentRefused("no answer".into()))?
        .map_err(|e| KeyError::AgentRefused(e.to_string()).into())
}

fn agent_key(identity: AgentIdentity) -> AgentKey {
    match identity {
        AgentIdentity::PublicKey { key, comment } => AgentKey {
            algorithm: key.algorithm().to_string(),
            fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
            comment: safe_comment(&comment),
            certificate: None,
        },
        AgentIdentity::Certificate {
            certificate,
            comment,
        } => {
            let key = PublicKey::from(certificate.public_key().clone());
            AgentKey {
                algorithm: certificate.algorithm().to_string(),
                fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
                comment: safe_comment(&comment),
                certificate: Some(AgentCertificate {
                    key_id: safe_comment(certificate.key_id()),
                    principals: certificate
                        .valid_principals()
                        .iter()
                        .map(|p| safe_comment(p))
                        .collect(),
                    valid_after: certificate.valid_after(),
                    valid_before: certificate.valid_before(),
                }),
            }
        }
    }
}

/// Agent-supplied text is shown as is: strip what could disguise it.
fn safe_comment(text: &str) -> String {
    crate::sftp::display_safe(text, 256).into_owned()
}

fn load_private(path: &Path, passphrase: Option<&SecretString>) -> Result<PrivateKey> {
    let bad = |reason: String| KeyError::BadPrivateKey {
        path: path.to_path_buf(),
        reason,
    };
    let text =
        zeroize::Zeroizing::new(std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?);
    let key = PrivateKey::from_openssh(text.as_str()).map_err(|e| bad(e.to_string()))?;
    if !key.is_encrypted() {
        return Ok(key);
    }
    let passphrase = passphrase
        .filter(|p| !p.expose_secret().is_empty())
        .ok_or_else(|| KeyError::NeedsPassphrase {
            path: path.to_path_buf(),
        })?;
    key.decrypt(passphrase.expose_secret().as_bytes())
        .map_err(|_| {
            KeyError::WrongPassphrase {
                path: path.to_path_buf(),
            }
            .into()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{KeyAlgorithm, generate};

    #[test]
    fn private_keys_load_with_their_passphrase_only() {
        let dir = tempfile::tempdir().unwrap();
        let req = |name: &str, pass: Option<&str>| KeyGenRequest {
            name: name.into(),
            algorithm: KeyAlgorithm::Ed25519,
            comment: String::new(),
            passphrase: pass.map(|p| SecretString::from(p.to_owned())),
        };
        let plain = generate(dir.path(), &req("plain", None)).unwrap();
        let locked = generate(dir.path(), &req("locked", Some("s3cret"))).unwrap();
        assert!(load_private(&plain.private_path, None).is_ok());
        let code = |r: Result<PrivateKey>| r.unwrap_err().summary().code;
        assert_eq!(
            code(load_private(&locked.private_path, None)),
            "key_needs_passphrase"
        );
        let wrong = SecretString::from("nope".to_owned());
        assert_eq!(
            code(load_private(&locked.private_path, Some(&wrong))),
            "key_wrong_passphrase"
        );
        let right = SecretString::from("s3cret".to_owned());
        assert!(load_private(&locked.private_path, Some(&right)).is_ok());
        assert_eq!(code(load_private(&locked.public_path, None)), "key_private");
    }

    #[test]
    fn certificate_validity_bands() {
        let now = 1_000_000;
        assert_eq!(
            cert_validity(now + 10, u64::MAX, now),
            CertValidity::NotYetValid { starts_in_secs: 10 }
        );
        assert_eq!(
            cert_validity(0, u64::MAX, now),
            CertValidity::Valid {
                expires_in_secs: None
            }
        );
        assert_eq!(
            cert_validity(0, now + 3600, now),
            CertValidity::ExpiresSoon {
                expires_in_secs: 3600
            }
        );
        assert_eq!(
            cert_validity(0, now, now),
            CertValidity::Expired { since_secs: 0 }
        );
        assert!(matches!(
            cert_validity(0, now + 30 * 24 * 3600, now),
            CertValidity::Valid {
                expires_in_secs: Some(_)
            }
        ));
    }
}
