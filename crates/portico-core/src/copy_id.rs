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

use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::events::{EngineEvent, SessionEvent, SessionPhase};
use crate::ids::SessionId;
use crate::keys::KeyError;
use crate::sftp::display_safe;

/// Whole exchange (channel open, script, exit status).
const COPY_TIMEOUT: Duration = Duration::from_secs(30);
/// Bytes of remote output kept (the end of it: the marker comes last).
const OUTPUT_LIMIT: usize = 4096;
/// Prefix of the result line printed by [`SCRIPT`].
const MARKER: &str = "portico-copy-id:";

/// Reads the key line from stdin, appends it unless its blob is present.
/// Prints `portico-copy-id:added` or `…:present` (a marker line: login
/// shell rc files may print their own text first). Exit codes ≥ 3 name the
/// failing step. Permissions of existing files are left alone (`umask 077`
/// covers what it creates); labels are restored with `restorecon` if present.
///
/// It is sent inside single quotes (`exec sh -c '…'`) to whatever login
/// shell the account has, so it is **one line** (csh/tcsh reject a newline
/// inside quotes) without `'`, `!` (csh history expansion, even quoted) or
/// `\\` (fish). A key counts as present only on a line that is not a
/// comment, as the field right after a key type (options may precede it).
const SCRIPT: &str = concat!(
    "umask 077; set -f; ",
    "IFS= read -r key || exit 3; set -- $key; [ $# -ge 2 ] || exit 3; blob=$2; ",
    "[ -n \"$HOME\" ] || exit 4; dir=\"$HOME/.ssh\"; file=\"$dir/authorized_keys\"; ",
    "mkdir -p \"$dir\" || exit 4; ",
    "if [ -f \"$file\" ] && awk -v b=\"$blob\" ",
    "\"/^[ \\t]*#/ { next } { for (i = 2; i <= NF; i++) ",
    "if (\\$i == b && \\$(i - 1) ~ /^(ssh-|ecdsa-sha2-|sk-)/) found = 1 } ",
    "END { exit (found ? 0 : 1) }\" \"$file\"; then echo portico-copy-id:present; exit 0; fi; ",
    "if [ -s \"$file\" ] && [ -n \"$(tail -c 1 \"$file\")\" ]; then ",
    "printf \"\\n\" >> \"$file\" || exit 5; fi; ",
    "printf \"%s\\n\" \"$key\" >> \"$file\" || exit 5; ",
    "command -v restorecon >/dev/null 2>&1 && restorecon -F \"$dir\" \"$file\" >/dev/null 2>&1; ",
    "echo portico-copy-id:added",
);

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
    if line
        .split_whitespace()
        .next()
        .is_some_and(|kind| kind.ends_with("-cert-v01@openssh.com"))
    {
        return Err(bad("this is a certificate, not a key").into());
    }
    let key = PublicKey::from_openssh(line).map_err(|e| bad(&e.to_string()))?;
    let mut key = key;
    // The comment is free text: keep it on one safe line.
    let comment: String = key
        .comment()
        .to_string()
        .chars()
        .filter(|c| !c.is_control() && !crate::keys::is_bidi_control(*c))
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
        let command = format!("exec sh -c '{SCRIPT}'");
        let opened = tokio::select! {
            biased;
            () = attached.ctx.cancel.cancelled() => return Err(Error::SessionEnded(session)),
            r = tokio::time::timeout(COPY_TIMEOUT, attached.handle.channel_open_session()) => r,
        };
        let mut channel = opened
            .map_err(|_| Error::Timeout {
                phase: "opening a channel on",
                target: host.clone(),
            })?
            .map_err(|e| failed(e.to_string()))?;
        let outcome = tokio::select! {
            biased;
            () = attached.ctx.cancel.cancelled() => Err(Error::SessionEnded(session)),
            r = tokio::time::timeout(COPY_TIMEOUT, exchange(&mut channel, &command, &line)) => {
                match r {
                    Ok(Ok(result)) => result.outcome().map_err(|reason| match reason {
                        Unexpected::Output => KeyError::CopyUnexpectedOutput { host: host.clone() }.into(),
                        Unexpected::Failed(reason) => failed(reason),
                    }),
                    Ok(Err(e)) => Err(failed(e)),
                    Err(_) => Err(Error::Timeout { phase: "installing the key on", target: host.clone() }),
                }
            }
        };
        // Always: a dropped russh channel stays open on the server.
        let _ = channel.close().await;
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

    /// Logs in again to the account of `session` (same alias and user)
    /// with `private_key` only, then disconnects. A passphrase prompt may
    /// appear (as for any login). Proves copy-id. Dropping the future
    /// closes the verification session.
    pub async fn verify_key_login(&self, session: SessionId, private_key: PathBuf) -> Result<()> {
        let mut request = self
            .session_target(session)
            .ok_or(Error::UnknownSession(session))?;
        let host = request.alias.clone();
        request.only_identity = Some(private_key);
        let mut events = self.subscribe();
        let id = self.connect(request)?;
        let _close = ProbeGuard(self.session_cancel(id));
        loop {
            match events.recv().await {
                Ok(EngineEvent::Session {
                    id: event_id,
                    event: SessionEvent::Phase(phase),
                }) if event_id == id => match phase {
                    SessionPhase::Connected => return Ok(()),
                    SessionPhase::Failed { error } if error.code == "auth_failed" => {
                        return Err(KeyError::NotAccepted { host }.into());
                    }
                    SessionPhase::Failed { error } => {
                        return Err(KeyError::ProbeFailed { summary: error }.into());
                    }
                    SessionPhase::Closed { .. } => return Err(Error::Cancelled),
                    _ => {}
                },
                Ok(_) => {}
                // The outcome may have been among the lost events: ask the
                // session itself (connected, or gone).
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    if let Some(connected) = self.settled(id).await {
                        return if connected {
                            Ok(())
                        } else {
                            Err(KeyError::NotAccepted { host }.into())
                        };
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(Error::EngineClosed);
                }
            }
        }
    }

    /// `Some(true)` once `id` is connected, `Some(false)` once it is gone,
    /// `None` while it is still connecting.
    async fn settled(&self, id: SessionId) -> Option<bool> {
        match tokio::time::timeout(Duration::from_millis(50), self.attach(id)).await {
            Ok(Ok(_)) => Some(true),
            Ok(Err(_)) => Some(false),
            Err(_) => None,
        }
    }
}

/// Ends the verification session however `verify_key_login` returns.
struct ProbeGuard(Option<tokio_util::sync::CancellationToken>);

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        if let Some(token) = &self.0 {
            token.cancel();
        }
    }
}

/// What came back from the server.
#[derive(Debug, Default)]
struct Exchange {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status: Option<u32>,
    signal: Option<String>,
}

enum Unexpected {
    /// Exit 0 without our marker: forced command, restricted account…
    Output,
    Failed(String),
}

impl Exchange {
    fn outcome(&self) -> std::result::Result<CopyIdOutcome, Unexpected> {
        let out = String::from_utf8_lossy(&self.stdout);
        let marker = out
            .lines()
            .rev()
            .find_map(|l| l.trim().strip_prefix(MARKER))
            .map(str::to_owned);
        if let Some(signal) = &self.signal {
            return Err(Unexpected::Failed(format!(
                "the command was killed ({signal})"
            )));
        }
        match (self.status, marker.as_deref()) {
            (Some(0) | None, Some("added")) => Ok(CopyIdOutcome::Added),
            (Some(0) | None, Some("present")) => Ok(CopyIdOutcome::AlreadyPresent),
            (Some(0), _) => Err(Unexpected::Output),
            (Some(code), _) => Err(Unexpected::Failed(step_failure(
                code,
                &String::from_utf8_lossy(&self.stderr),
            ))),
            (None, _) => Err(Unexpected::Failed(
                "the command ended without a status".into(),
            )),
        }
    }

    fn take(&mut self, msg: ChannelMsg) {
        match msg {
            ChannelMsg::Data { data } => keep_tail(&mut self.stdout, &data),
            ChannelMsg::ExtendedData { data, .. } => keep_tail(&mut self.stderr, &data),
            ChannelMsg::ExitStatus { exit_status } => self.status = Some(exit_status),
            ChannelMsg::ExitSignal { signal_name, .. } => {
                self.signal = Some(format!("{signal_name:?}"));
            }
            _ => {}
        }
    }
}

/// Runs `command`, feeds `line` on its stdin once the server accepted it,
/// and collects the outcome until the channel closes.
async fn exchange(
    channel: &mut russh::Channel<russh::client::Msg>,
    command: &str,
    line: &str,
) -> std::result::Result<Exchange, String> {
    let mut result = Exchange::default();
    channel
        .exec(true, command.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    // Data sent before the answer could be dropped by a server that has
    // not started the command yet.
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Success) => break,
            Some(ChannelMsg::Failure) => return Err("the server refused to run a command".into()),
            Some(ChannelMsg::Close | ChannelMsg::Eof) | None => {
                return Err("the server closed the channel".into());
            }
            Some(msg) => result.take(msg),
        }
    }
    channel
        .data(format!("{line}\n").as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    channel.eof().await.map_err(|e| e.to_string())?;
    while let Some(msg) = channel.wait().await {
        if matches!(msg, ChannelMsg::Close) {
            break;
        }
        result.take(msg);
    }
    Ok(result)
}

/// Keeps the last [`OUTPUT_LIMIT`] bytes (the stream keeps being drained, so
/// the SSH window keeps opening).
fn keep_tail(buffer: &mut Vec<u8>, data: &[u8]) {
    buffer.extend_from_slice(data);
    let excess = buffer.len().saturating_sub(OUTPUT_LIMIT);
    if excess > 0 {
        buffer.drain(..excess);
    }
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
        let stdout = String::from_utf8_lossy(&out.stdout);
        let result = stdout
            .lines()
            .find_map(|l| l.strip_prefix(MARKER))
            .unwrap_or_default()
            .to_owned();
        (out.status.code().unwrap_or(-1), result)
    }

    #[test]
    fn script_survives_any_login_shell_quoting() {
        assert!(!SCRIPT.contains('\''));
        assert!(!SCRIPT.contains('!'));
        assert!(!SCRIPT.contains('\n'));
        assert!(!SCRIPT.contains("\\\\"));
    }

    /// The exact command sent to the server, through each login shell that
    /// is installed here (sh always; bash, fish, tcsh, csh when present).
    #[cfg(unix)]
    #[test]
    fn command_runs_under_every_login_shell() {
        use std::io::Write;
        let command = format!("exec sh -c '{SCRIPT}'");
        for shell in ["sh", "bash", "dash", "fish", "tcsh", "csh", "zsh"] {
            let Ok(mut child) = std::process::Command::new(shell)
                .arg("-c")
                .arg(&command)
                .env("HOME", tempfile::tempdir().unwrap().keep())
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
            else {
                continue; // not installed
            };
            child
                .stdin
                .take()
                .unwrap()
                .write_all(format!("{KEY} me\n").as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                "portico-copy-id:added",
                "{shell}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn disabled_or_quoted_keys_do_not_count() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".ssh")).unwrap();
        let file = home.path().join(".ssh/authorized_keys");
        let blob = KEY.split_whitespace().nth(1).unwrap();
        std::fs::write(
            &file,
            format!("#{KEY} off\n  # {KEY}\nssh-rsa AAAAx {blob}\n"),
        )
        .unwrap();
        assert_eq!(
            run(home.path(), &format!("{KEY} me\n")),
            (0, "added".into())
        );
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .ends_with(&format!("{KEY} me\n"))
        );
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
        // An existing file keeps its permissions (never chmod what we did
        // not create).
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        let other =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOtherOtherOtherOtherOtherOtherOtherOther0";
        assert_eq!(run(home.path(), &format!("{other} x\n")).1, "added");
        assert_eq!(mode(&file), 0o640);
        std::fs::write(&file, format!("{KEY} me@laptop\n")).unwrap();
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
    fn outcome_reads_the_marker_whatever_comes_before() {
        let ex = |out: &str, status: Option<u32>, signal: Option<&str>| Exchange {
            stdout: out.as_bytes().to_vec(),
            stderr: b"sh: 1: cannot create: Permission denied\n".to_vec(),
            status,
            signal: signal.map(str::to_owned),
        };
        let ok = |e: Exchange| e.outcome().ok();
        assert_eq!(
            ok(ex(
                "Welcome!\nhello from rc\nportico-copy-id:added\n",
                Some(0),
                None
            )),
            Some(CopyIdOutcome::Added)
        );
        assert_eq!(
            ok(ex("portico-copy-id:present\n", None, None)),
            Some(CopyIdOutcome::AlreadyPresent)
        );
        // Forced command or restricted account: exit 0, no marker.
        assert!(matches!(
            ex("This account is restricted\n", Some(0), None).outcome(),
            Err(Unexpected::Output)
        ));
        assert!(matches!(
            ex("", Some(5), None).outcome(),
            Err(Unexpected::Failed(m)) if m.contains("authorized_keys") && m.contains("Permission denied")
        ));
        assert!(matches!(
            ex("portico-copy-id:added\n", Some(0), Some("KILL")).outcome(),
            Err(Unexpected::Failed(_))
        ));
        // Huge output: only the end is kept, and the marker survives.
        let mut big = Exchange::default();
        keep_tail(&mut big.stdout, &vec![b'x'; 1 << 20]);
        keep_tail(&mut big.stdout, b"\nportico-copy-id:added\n");
        big.status = Some(0);
        assert!(big.stdout.len() <= OUTPUT_LIMIT);
        assert_eq!(ok(big), Some(CopyIdOutcome::Added));
    }

    #[test]
    fn comments_lose_bidi_controls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("k.pub");
        std::fs::write(&path, format!("{KEY} me\u{202e}moc.live\n")).unwrap();
        assert_eq!(authorized_line(&path).unwrap(), format!("{KEY} memoc.live"));
        // A comment that merely mentions a certificate type is a key.
        std::fs::write(
            &path,
            format!("{KEY} not-a-ssh-ed25519-cert-v01@openssh.com\n"),
        )
        .unwrap();
        assert!(authorized_line(&path).is_ok());
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
