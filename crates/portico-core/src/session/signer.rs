//! Lazy signers for public key authentication.
//!
//! russh first *probes* a public key (`publickey` request without signature)
//! and asks the signer only after the server answered `PK_OK`. The file signer
//! therefore decrypts a private key — asking for its passphrase if needed —
//! **only when the server actually accepts that key**, like OpenSSH.

use std::path::PathBuf;

use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::signature::Signer as _;
use russh::keys::ssh_encoding::Encode as _;
use russh::keys::ssh_key::private::KeypairData;
use russh::keys::{HashAlg, PrivateKey};
use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroizing;

use super::SessionCtx;
use crate::engine::PromptResponse;
use crate::error::Error;
use crate::events::PromptKind;
use crate::logs::LogLevel;
use crate::secrets::{SecretKey, SecretStore};

/// Passphrase attempts before giving up on a key (OpenSSH: 3).
const PASSPHRASE_ATTEMPTS: u32 = 3;

pub(crate) type Agent = AgentClient<Box<dyn AgentStream + Send + Unpin + 'static>>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum SignError {
    #[error(transparent)]
    Send(#[from] russh::SendError),
    #[error("{0}")]
    Key(String),
    #[error("cancelled")]
    Cancelled,
}

/// What happened when the server asked for a signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Server rejected the key at probe time: no signature requested.
    NotRequested,
    Signed,
    /// The key could not be unlocked (dismissed or wrong passphrase).
    Skipped,
}

pub(crate) enum KeySigner<'a> {
    Agent {
        agent: &'a mut Agent,
        ctx: &'a SessionCtx,
        outcome: Outcome,
    },
    File(FileSigner<'a>),
}

impl<'a> KeySigner<'a> {
    pub(crate) fn agent(agent: &'a mut Agent, ctx: &'a SessionCtx) -> Self {
        Self::Agent {
            agent,
            ctx,
            outcome: Outcome::NotRequested,
        }
    }

    pub(crate) fn outcome(&self) -> Outcome {
        match self {
            Self::Agent { outcome, .. } => *outcome,
            Self::File(file) => file.outcome,
        }
    }
}

pub(crate) struct FileSigner<'a> {
    ctx: &'a SessionCtx,
    path: PathBuf,
    fingerprint: String,
    pub(crate) outcome: Outcome,
}

impl<'a> FileSigner<'a> {
    pub(crate) fn new(ctx: &'a SessionCtx, path: PathBuf, fingerprint: String) -> Self {
        Self {
            ctx,
            path,
            fingerprint,
            outcome: Outcome::NotRequested,
        }
    }

    async fn sign(
        &mut self,
        identity: &AgentIdentity,
        hash_alg: Option<HashAlg>,
        to_sign: Vec<u8>,
    ) -> Result<Vec<u8>, SignError> {
        let Some(key) = self.unlock().await? else {
            self.outcome = Outcome::Skipped;
            return refusal(identity, hash_alg, to_sign);
        };
        self.outcome = Outcome::Signed;
        append_signature(&key, hash_alg, to_sign)
    }

    /// Stores a passphrase that just decrypted the key. A failure is only
    /// logged, with the error code and never the secret.
    async fn remember(
        &self,
        store: &std::sync::Arc<dyn SecretStore>,
        secret_key: &SecretKey,
        secret: SecretString,
    ) {
        let (store, secret_key) = (std::sync::Arc::clone(store), secret_key.clone());
        let saved = tokio::task::spawn_blocking(move || store.set(&secret_key, &secret)).await;
        match saved {
            Ok(Ok(())) => {}
            Ok(Err(e)) => self.ctx.log(
                LogLevel::Warn,
                "secrets",
                format!("could not remember the passphrase ({})", e.summary().code),
            ),
            Err(_) => self.ctx.log(
                LogLevel::Warn,
                "secrets",
                "could not remember the passphrase",
            ),
        }
    }

    /// Loads the private key, asking for a passphrase if it is encrypted.
    /// `Ok(None)` means "skip this key".
    async fn unlock(&self) -> Result<Option<PrivateKey>, SignError> {
        let text =
            Zeroizing::new(tokio::fs::read_to_string(&self.path).await.map_err(|e| {
                SignError::Key(format!("cannot read {}: {e}", self.path.display()))
            })?);
        let encrypted = match PrivateKey::from_openssh(text.as_bytes()) {
            Ok(key) if !key.is_encrypted() => return Ok(Some(key)),
            Ok(key) => Encrypted::OpenSsh(Box::new(key)),
            Err(_) => match russh::keys::decode_secret_key(&text, None) {
                Ok(key) => return Ok(Some(key)),
                Err(russh::keys::Error::KeyIsEncrypted) => Encrypted::Legacy(text),
                Err(e) => {
                    return Err(SignError::Key(format!(
                        "cannot parse {}: {e}",
                        self.path.display()
                    )));
                }
            },
        };

        let store = std::sync::Arc::clone(&self.ctx.engine.config().secrets);
        let secret_key = SecretKey::KeyPassphrase {
            fingerprint: self.fingerprint.clone(),
        };
        let lookup = {
            let (store, secret_key) = (std::sync::Arc::clone(&store), secret_key.clone());
            tokio::task::spawn_blocking(move || store.get(&secret_key)).await
        };
        if let Ok(Ok(Some(remembered))) = lookup {
            if let Some(key) = encrypted.decrypt(remembered).await {
                self.ctx.log(
                    LogLevel::Info,
                    "auth",
                    "key unlocked with the remembered passphrase",
                );
                return Ok(Some(key));
            }
            self.ctx.log(
                LogLevel::Warn,
                "auth",
                "remembered passphrase no longer works",
            );
        }

        for attempt in 1..=PASSPHRASE_ATTEMPTS {
            let answer = self
                .ctx
                .ask(PromptKind::KeyPassphrase {
                    key_path: self.path.display().to_string(),
                    attempt,
                    attempts: PASSPHRASE_ATTEMPTS,
                })
                .await;
            match answer {
                Ok(PromptResponse::Secret { value, remember }) => {
                    let to_store = remember.then(|| value.clone());
                    if let Some(key) = encrypted.decrypt(value).await {
                        if let Some(secret) = to_store {
                            self.remember(&store, &secret_key, secret).await;
                        }
                        return Ok(Some(key));
                    }
                    self.ctx.log(
                        LogLevel::Warn,
                        "auth",
                        format!(
                            "wrong passphrase for {} ({attempt}/{PASSPHRASE_ATTEMPTS})",
                            self.path.display()
                        ),
                    );
                }
                Ok(_) => {
                    self.ctx.log(
                        LogLevel::Info,
                        "auth",
                        "passphrase prompt dismissed: skipping this key",
                    );
                    return Ok(None);
                }
                Err(Error::Cancelled) => return Err(SignError::Cancelled),
                Err(e) => return Err(SignError::Key(e.to_string())),
            }
        }
        Ok(None)
    }
}

enum Encrypted {
    OpenSsh(Box<PrivateKey>),
    /// PKCS#1/PKCS#8 PEM, kept as text for `decode_secret_key`.
    Legacy(Zeroizing<String>),
}

impl Encrypted {
    /// Decrypts off the async runtime (bcrypt-pbkdf is deliberately slow).
    async fn decrypt(&self, passphrase: SecretString) -> Option<PrivateKey> {
        match self {
            Self::OpenSsh(key) => {
                let key = key.clone();
                tokio::task::spawn_blocking(move || {
                    key.decrypt(passphrase.expose_secret().as_bytes()).ok()
                })
                .await
                .ok()
                .flatten()
            }
            Self::Legacy(text) => {
                let text = text.clone();
                tokio::task::spawn_blocking(move || {
                    russh::keys::decode_secret_key(&text, Some(passphrase.expose_secret())).ok()
                })
                .await
                .ok()
                .flatten()
            }
        }
    }
}

/// Appends the SSH-encoded signature of `to_sign` (the format russh expects
/// back from a signer).
fn append_signature(
    key: &PrivateKey,
    hash_alg: Option<HashAlg>,
    mut to_sign: Vec<u8>,
) -> Result<Vec<u8>, SignError> {
    let signature = match key.key_data() {
        KeypairData::Rsa(keypair) => (keypair, hash_alg).try_sign(&to_sign),
        keypair => keypair.try_sign(&to_sign),
    }
    .map_err(|e| SignError::Key(format!("signing failed: {e}")))?;
    let blob = signature
        .encode_vec()
        .map_err(|e| SignError::Key(e.to_string()))?;
    blob.as_slice()
        .encode(&mut to_sign)
        .map_err(|e| SignError::Key(e.to_string()))?;
    Ok(to_sign)
}

/// Appends a *well-formed but invalid* signature (right algorithm name and
/// size, all zeros). russh 0.64 cannot abandon a request after `PK_OK`, so
/// this is how a key is skipped (passphrase dismissed, agent refusal): every
/// server parses it and answers with a plain auth failure.
fn refusal(
    identity: &AgentIdentity,
    hash_alg: Option<HashAlg>,
    mut to_sign: Vec<u8>,
) -> Result<Vec<u8>, SignError> {
    use russh::keys::ssh_key::public::KeyData;

    let key_data = match identity {
        AgentIdentity::PublicKey { key, .. } => key.key_data().clone(),
        AgentIdentity::Certificate { certificate, .. } => certificate.public_key().clone(),
    };
    let (name, data): (String, Vec<u8>) = match &key_data {
        KeyData::Rsa(rsa) => (
            russh::keys::Algorithm::Rsa { hash: hash_alg }
                .as_str()
                .to_owned(),
            vec![0; rsa.n().as_positive_bytes().map_or(256, <[u8]>::len)],
        ),
        KeyData::Ecdsa(ecdsa) => (
            russh::keys::Algorithm::Ecdsa {
                curve: ecdsa.curve(),
            }
            .as_str()
            .to_owned(),
            // string(mpint 1) || string(mpint 1)
            vec![0, 0, 0, 1, 1, 0, 0, 0, 1, 1],
        ),
        other => (other.algorithm().as_str().to_owned(), vec![0; 64]),
    };
    let mut blob = Vec::new();
    name.as_bytes()
        .encode(&mut blob)
        .and_then(|()| data.as_slice().encode(&mut blob))
        .and_then(|()| blob.as_slice().encode(&mut to_sign))
        .map_err(|e| SignError::Key(e.to_string()))?;
    Ok(to_sign)
}

impl russh::Signer for KeySigner<'_> {
    type Error = SignError;

    async fn auth_sign(
        &mut self,
        key: &AgentIdentity,
        hash_alg: Option<HashAlg>,
        to_sign: Vec<u8>,
    ) -> Result<Vec<u8>, Self::Error> {
        match self {
            Self::Agent {
                agent,
                ctx,
                outcome,
            } => match agent.sign_request(key, hash_alg, to_sign.clone()).await {
                Ok(signed) => {
                    *outcome = Outcome::Signed;
                    Ok(signed)
                }
                Err(e) => {
                    // Refused confirmation, cancelled Touch ID, FIDO timeout…:
                    // move on to the next identity like `ssh` does.
                    ctx.log(LogLevel::Warn, "agent", format!("agent did not sign: {e}"));
                    *outcome = Outcome::Skipped;
                    refusal(key, hash_alg, to_sign)
                }
            },
            Self::File(file) => file.sign(key, hash_alg, to_sign).await,
        }
    }
}
