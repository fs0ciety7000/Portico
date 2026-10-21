//! `ssh-copy-id`: installs a public key in the remote `authorized_keys` over
//! an authenticated session, then proves it works by logging in again with
//! that key only.
//!
//! The key travels on the **stdin** of an `exec` channel, never in the
//! command line (no quoting, no injection, nothing in the server's process
//! list). The remote script is POSIX `sh`, run through `exec sh -c` so a
//! fish or csh login shell does not matter. It is idempotent: a key whose
//! blob is already present (whatever its options or comment) is not added
//! again. It creates `~/.ssh` (`0700`) and `authorized_keys` (`0600`) under
//! `umask 077` and never rewrites existing lines.

use std::path::{Path, PathBuf};
use std::time::Duration;

use russh::ChannelMsg;
use russh::keys::ssh_key::PublicKey;
use serde::{Deserialize, Serialize};

use crate::engine::{ConnectRequest, Engine};
use crate::error::{Error, Result};
use crate::events::{EngineEvent, SessionEvent, SessionPhase};
use crate::ids::SessionId;
use crate::keys::KeyError;
use crate::sftp::display_safe;

/// Whole exchange (channel open, script, exit status).
const COPY_TIMEOUT: Duration = Duration::from_secs(30);
/// Bytes of remote output kept for the error message.
const OUTPUT_LIMIT: usize = 2048;

/// Reads the key line from stdin, appends it unless its blob is present.
/// Prints `added` or `present`. Exit codes ≥ 3 name the failing step.
/// It is sent inside single quotes (`sh -c '…'`): it must contain no `'`,
/// and no `\\` (fish gives it a meaning inside single quotes).
const SCRIPT: &str = r#"umask 077
set -f
IFS= read -r key || exit 3
set -- $key
[ $# -ge 2 ] || exit 3
blob=$2
dir="$HOME/.ssh"
file="$dir/authorized_keys"
mkdir -p "$dir" || exit 4
chmod 700 "$dir" 2>/dev/null
if [ -f "$file" ] && awk -v b="$blob" "{ for (i = 1; i <= NF; i++) if (\$i == b) found = 1 } END { exit !found }" "$file"; then
  echo present
  exit 0
fi
if [ -s "$file" ] && [ -n "$(tail -c 1 "$file")" ]; then
  printf "\n" >> "$file" || exit 5
fi
printf "%s\n" "$key" >> "$file" || exit 5
chmod 600 "$file" 2>/dev/null
echo added
"#;

/// What copy-id did on the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyIdOutcome {
    Added,
    /// The key was already authorized: nothing changed.
    AlreadyPresent,
}

/// The single line written to `authorized_keys`: `type base64 comment`, from
/// a parsed key (never the raw file: a crafted `.pub` cannot smuggle options
/// or extra lines). Certificates are refused.
pub(crate) fn authorized_line(public_path: &Path) -> Result<String> {
    let bad = |reason: &str| KeyError::BadPublicKey {
        path: public_path.to_path_buf(),
        reason: reason.to_owned(),
    };
    let text = std::fs::read_to_string(public_path).map_err(|e| Error::io(public_path, e))?;
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let line = lines.next().ok_or_else(|| bad("empty file"))?;
    if lines.next().is_some() {
        return Err(bad("more than one key").into());
    }
    if line.contains("-cert-v01@openssh.com") {
        return Err(bad("this is a certificate, not a key").into());
    }
    let key = PublicKey::from_openssh(line).map_err(|e| bad(&e.to_string()))?;
    let mut key = key;
    // The comment is free text: keep it on one safe line.
    let comment: String = key
        .comment()
        .to_string()
        .chars()
        .filter(|c| !c.is_control())
        .take(256)
        .collect();
    key.set_comment(comment.as_str());
    key.to_openssh().map_err(|e| bad(&e.to_string()).into())
}

impl Engine {
    /// Installs the public key at `public_path` for the account of
    /// `session` (which must be authenticated by other means).
    pub async fn copy_id(&self, session: SessionId, public_path: PathBuf) -> Result<CopyIdOutcome> {
        let line = tokio::task::spawn_blocking(move || authorized_line(&public_path))
            .await
            .map_err(|_| Error::EngineClosed)??;
        let attached = self.attach(session).await?;
        let host = self.session_alias(session).unwrap_or_default();
        let failed = |reason: String| -> Error {
            KeyError::CopyFailed {
                host: host.clone(),
                reason: display_safe(&reason, 300).into_owned(),
            }
            .into()
        };
        let run = async {
            let mut channel = attached
                .handle
                .channel_open_session()
                .await
                .map_err(|e| failed(e.to_string()))?;
            let command = format!("exec sh -c '{SCRIPT}'");
            channel
                .exec(true, command.as_bytes())
                .await
                .map_err(|e| failed(e.to_string()))?;
            channel
                .data(format!("{line}\n").as_bytes())
                .await
                .map_err(|e| failed(e.to_string()))?;
            channel.eof().await.map_err(|e| failed(e.to_string()))?;
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let mut status = None;
            while let Some(msg) = channel.wait().await {
                match msg {
                    ChannelMsg::Failure => {
                        return Err(failed("the server refused to run a command".into()));
                    }
                    ChannelMsg::Data { data } => keep(&mut stdout, &data),
                    ChannelMsg::ExtendedData { data, .. } => keep(&mut stderr, &data),
                    ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                    ChannelMsg::ExitSignal { signal_name, .. } => {
                        return Err(failed(format!("the command was killed ({signal_name:?})")));
                    }
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
            let _ = channel.close().await;
            let out = String::from_utf8_lossy(&stdout);
            match (status, out.trim()) {
                (Some(0), "added") => Ok(CopyIdOutcome::Added),
                (Some(0), "present") => Ok(CopyIdOutcome::AlreadyPresent),
                (Some(code), _) => Err(failed(step_failure(
                    code,
                    &String::from_utf8_lossy(&stderr),
                ))),
                (None, _) => Err(failed("the command ended without a status".into())),
            }
        };
        let outcome = tokio::select! {
            biased;
            () = attached.ctx.cancel.cancelled() => Err(Error::SessionEnded(session)),
            r = tokio::time::timeout(COPY_TIMEOUT, run) => r.unwrap_or_else(|_| {
                Err(Error::Timeout { phase: "installing the key on", target: host.clone() })
            }),
        };
        match &outcome {
            Ok(o) => attached.ctx.log(
                crate::logs::LogLevel::Info,
                "copy-id",
                match o {
                    CopyIdOutcome::Added => "public key added to ~/.ssh/authorized_keys",
                    CopyIdOutcome::AlreadyPresent => "public key already in ~/.ssh/authorized_keys",
                },
            ),
            Err(e) => attached
                .ctx
                .log(crate::logs::LogLevel::Error, "copy-id", e.to_string()),
        }
        outcome
    }

    /// Logs in to `alias` with `private_key` only, then disconnects. A
    /// passphrase prompt may appear (as for any login). Proves copy-id.
    pub async fn verify_key_login(&self, alias: &str, private_key: PathBuf) -> Result<()> {
        let mut events = self.subscribe();
        let mut request = ConnectRequest::new(alias);
        request.only_identity = Some(private_key);
        let id = self.connect(request)?;
        let outcome = loop {
            match events.recv().await {
                Ok(EngineEvent::Session {
                    id: event_id,
                    event: SessionEvent::Phase(phase),
                }) if event_id == id => match phase {
                    SessionPhase::Connected => break Ok(()),
                    SessionPhase::Failed { error } if error.code == "auth_failed" => {
                        break Err(KeyError::NotAccepted {
                            host: alias.to_owned(),
                        }
                        .into());
                    }
                    SessionPhase::Failed { error } => {
                        break Err(KeyError::CopyFailed {
                            host: alias.to_owned(),
                            reason: error.message,
                        }
                        .into());
                    }
                    SessionPhase::Closed { .. } => break Err(Error::Cancelled),
                    _ => {}
                },
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break Err(Error::EngineClosed);
                }
            }
        };
        let _ = self.disconnect(id).await;
        outcome
    }
}

fn keep(buffer: &mut Vec<u8>, data: &[u8]) {
    let room = OUTPUT_LIMIT.saturating_sub(buffer.len());
    buffer.extend(data.iter().take(room));
}

fn step_failure(code: u32, stderr: &str) -> String {
    let step = match code {
        3 => "the key did not reach the server",
        4 => "cannot create ~/.ssh",
        5 => "cannot write ~/.ssh/authorized_keys",
        127 => "no POSIX shell (sh) on the server",
        _ => "the command failed",
    };
    let detail = stderr
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim();
    if detail.is_empty() {
        format!("{step} (exit {code})")
    } else {
        format!("{step}: {detail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFPhl4hO0pS1bxgFwR0cIGijellPogqzFcdSn+al5ZRJ";

    /// Runs the script like the server would (`sh -c`), key on stdin.
    #[cfg(unix)]
    fn run(home: &Path, stdin: &str) -> (i32, String) {
        use std::io::Write;
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(SCRIPT)
            .env("HOME", home)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        )
    }

    #[test]
    fn script_survives_single_quoting() {
        assert!(!SCRIPT.contains('\''));
        assert!(!SCRIPT.contains("\\\\"));
    }

    #[cfg(unix)]
    #[test]
    fn script_adds_once_with_private_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            run(home.path(), &format!("{KEY} me@laptop\n")),
            (0, "added".into())
        );
        let file = home.path().join(".ssh/authorized_keys");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&home.path().join(".ssh")), 0o700);
        assert_eq!(mode(&file), 0o600);
        // Same blob, other comment: already there.
        assert_eq!(
            run(home.path(), &format!("{KEY} other\n")),
            (0, "present".into())
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            format!("{KEY} me@laptop\n")
        );
    }

    #[cfg(unix)]
    #[test]
    fn script_keeps_existing_lines_and_options() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".ssh")).unwrap();
        let file = home.path().join(".ssh/authorized_keys");
        // No trailing newline, and the key already present behind options.
        std::fs::write(&file, "ssh-rsa AAAAother old").unwrap();
        assert_eq!(
            run(home.path(), &format!("{KEY} *glob* me\n")),
            (0, "added".into())
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            format!("ssh-rsa AAAAother old\n{KEY} *glob* me\n")
        );
        std::fs::write(&file, format!("no-pty,from=\"10.0.0.1\" {KEY} pinned\n")).unwrap();
        assert_eq!(run(home.path(), &format!("{KEY}\n")), (0, "present".into()));
    }

    #[cfg(unix)]
    #[test]
    fn script_rejects_an_empty_key() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(run(home.path(), "").0, 3);
        assert_eq!(run(home.path(), "justoneword\n").0, 3);
        assert!(!home.path().join(".ssh/authorized_keys").exists());
    }

    #[test]
    fn authorized_line_is_rebuilt_from_the_parsed_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("k.pub");
        std::fs::write(&path, format!("{KEY} me@laptop\n")).unwrap();
        assert_eq!(authorized_line(&path).unwrap(), format!("{KEY} me@laptop"));

        for (content, why) in [
            (String::new(), "empty"),
            (format!("{KEY} a\n{KEY} b\n"), "two keys"),
            (format!("command=\"rm -rf ~\" {KEY} evil\n"), "options"),
            (
                "ssh-ed25519-cert-v01@openssh.com AAAA x\n".to_owned(),
                "certificate",
            ),
            ("not a key\n".to_owned(), "garbage"),
        ] {
            std::fs::write(&path, content).unwrap();
            let err = authorized_line(&path).unwrap_err();
            assert_eq!(err.summary().code, "key_public", "{why}");
        }
    }
}
