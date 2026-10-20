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
use crate::secrets::SecretKey;

/// Passphrase attempts before giving up on a key (OpenSSH: 3).
const PASSPHRASE_ATTEMPTS: u32 = 3;

pub(crate) type Agent = AgentClient<Box<dyn AgentStream + Send + Unpin + 'static>>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum SignError {
    #[error(transparent)]
    Send(#[from] russh::SendError),
    #[error("ssh-agent refused to sign: {0}")]
    Agent(String),
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
    Agent(&'a mut Agent),
    File(FileSigner<'a>),
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
        hash_alg: Option<HashAlg>,
        to_sign: Vec<u8>,
    ) -> Result<Vec<u8>, SignError> {
        let Some(key) = self.unlock().await? else {
            self.outcome = Outcome::Skipped;
            return Ok(invalid_signature(to_sign));
        };
        self.outcome = Outcome::Signed;
        append_signature(&key, hash_alg, to_sign)
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
                })
                .await;
            match answer {
                Ok(PromptResponse::Secret { value, remember }) => {
                    let to_store = remember.then(|| value.clone());
                    if let Some(key) = encrypted.decrypt(value).await {
                        if let Some(secret) = to_store {
                            let (store, secret_key) =
                                (std::sync::Arc::clone(&store), secret_key.clone());
                            let saved = tokio::task::spawn_blocking(move || {
                                store.set(&secret_key, &secret)
                            })
                            .await;
                            if !matches!(saved, Ok(Ok(()))) {
                                self.ctx.log(
                                    LogLevel::Warn,
                                    "secrets",
                                    "could not remember the passphrase",
                                );
                            }
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

/// An empty signature: the server answers with a regular auth failure, which
/// keeps the protocol in sync when a key is skipped after `PK_OK`.
fn invalid_signature(mut to_sign: Vec<u8>) -> Vec<u8> {
    to_sign.extend_from_slice(&[0, 0, 0, 0]);
    to_sign
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
            Self::Agent(agent) => agent
                .sign_request(key, hash_alg, to_sign)
                .await
                .map_err(|e| SignError::Agent(e.to_string())),
            Self::File(file) => file.sign(hash_alg, to_sign).await,
        }
    }
}
