//! Interoperability with a real OpenSSH `sshd` (and `ssh-agent`, `ssh-keygen`).
//!
//! Ignored by default because it needs OpenSSH installed; CI runs it with
//! `cargo test -p portico-core --test openssh_interop -- --ignored`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Test orchestration: one long scenario script, child processes killed on drop.
#![allow(clippy::too_many_lines, clippy::zombie_processes)]

mod support;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use portico_core::engine::PromptResponse;
use portico_core::events::{AuthMethod, PromptKind, SessionPhase};
use secrecy::SecretString;
use support::{Env, accept_host, never};

fn run(cmd: &mut Command) {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{cmd:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn keygen(path: &Path, kind: &str, passphrase: &str) {
    run(Command::new("ssh-keygen")
        .args([
            "-q",
            "-t",
            kind,
            "-N",
            passphrase,
            "-C",
            "portico-test",
            "-f",
        ])
        .arg(path));
}

fn sign(ca: &Path, key_pub: &Path, principal: &str, host: bool) {
    let mut cmd = Command::new("ssh-keygen");
    cmd.args(["-q", "-s"])
        .arg(ca)
        .args(["-I", "portico", "-n", principal]);
    if host {
        cmd.arg("-h");
    }
    run(cmd.arg(key_pub));
}

fn current_user() -> String {
    String::from_utf8(Command::new("id").arg("-un").output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_owned()
}

struct Sshd {
    child: Child,
    port: u16,
    dir: tempfile::TempDir,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

impl Sshd {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        keygen(&d.join("host_ed25519"), "ed25519", "");
        keygen(&d.join("host_ca"), "ed25519", "");
        keygen(&d.join("user_ca"), "ed25519", "");
        sign(
            &d.join("host_ca"),
            &d.join("host_ed25519.pub"),
            "127.0.0.1",
            true,
        );
        std::fs::write(d.join("authorized_keys"), "").unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let config = format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {d}/host_ed25519\n\
             HostCertificate {d}/host_ed25519-cert.pub\nPidFile {d}/sshd.pid\n\
             AuthorizedKeysFile {d}/authorized_keys\nTrustedUserCAKeys {d}/user_ca.pub\n\
             StrictModes no\nUsePAM no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n\
             PermitRootLogin yes\nAllowTcpForwarding yes\nLogLevel DEBUG\n",
            d = d.display()
        );
        std::fs::write(d.join("sshd_config"), config).unwrap();
        // Privilege separation directory (only needed when running as root).
        let _ = std::fs::create_dir_all("/run/sshd");
        let log = std::fs::File::create(d.join("sshd.log")).unwrap();
        let child = Command::new("/usr/sbin/sshd")
            .args(["-D", "-e", "-f"])
            .arg(d.join("sshd_config"))
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        for _ in 0..100 {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                return Self { child, port, dir };
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!(
            "sshd did not start: {}",
            std::fs::read_to_string(d.join("sshd.log")).unwrap_or_default()
        );
    }

    fn authorize(&self, pub_key: &Path) {
        let mut keys = std::fs::read_to_string(self.path("authorized_keys")).unwrap();
        keys.push_str(&std::fs::read_to_string(pub_key).unwrap());
        std::fs::write(self.path("authorized_keys"), keys).unwrap();
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.path("sshd.log")).unwrap_or_default()
    }
}

fn host(alias: &str, port: u16, user: &str, extra: &str) -> String {
    format!("Host {alias}\n  HostName 127.0.0.1\n  Port {port}\n  User {user}\n{extra}")
}

fn assert_connected(out: &support::Outcome, sshd: &Sshd) {
    assert!(
        out.connected(),
        "{:?}\n--- sshd log ---\n{}",
        out.phases,
        sshd.log()
    );
}

#[tokio::test]
#[ignore = "needs OpenSSH (sshd, ssh-keygen, ssh-agent); run with --ignored"]
async fn openssh_interop() {
    let sshd = Sshd::start().await;
    let user = current_user();
    let port = sshd.port;

    // 1. TOFU + ed25519 key + latency.
    let mut env = Env::new(&host(
        "t",
        port,
        &user,
        "  IdentityFile ~/.ssh/id_ed25519\n",
    ));
    keygen(&env.ssh("id_ed25519"), "ed25519", "");
    sshd.authorize(&env.ssh("id_ed25519.pub"));
    let (id, out) = env.connect("t", accept_host).await;
    assert_connected(&out, &sshd);
    assert_eq!(out.prompts.len(), 1);

    // 1b. Real PTY + login shell: arithmetic proves a shell evaluated it.
    let size = portico_core::engine::PtySize {
        cols: 80,
        rows: 24,
        pixel_width: 0,
        pixel_height: 0,
    };
    let mut term = env.engine.open_terminal(id, size).await.unwrap();
    term.input
        .send(bytes::Bytes::from_static(b"echo portico-$((40+2))\r"))
        .await
        .unwrap();
    let mut seen = Vec::new();
    while !String::from_utf8_lossy(&seen).contains("portico-42") {
        let chunk = tokio::time::timeout(Duration::from_secs(5), term.output.recv())
            .await
            .expect("shell output in time")
            .expect("terminal open");
        seen.extend_from_slice(&chunk);
    }

    // 2. RSA key: rsa-sha2-* negotiated through server-sig-algs.
    let mut env = Env::new(&host("t", port, &user, "  IdentityFile ~/.ssh/id_rsa\n"));
    keygen(&env.ssh("id_rsa"), "rsa", "");
    sshd.authorize(&env.ssh("id_rsa.pub"));
    let (_, out) = env.connect("t", accept_host).await;
    assert_connected(&out, &sshd);

    // 3. Encrypted key (OpenSSH bcrypt-pbkdf) + passphrase prompt.
    let mut env = Env::new(&host("t", port, &user, "  IdentityFile ~/.ssh/id_locked\n"));
    keygen(&env.ssh("id_locked"), "ed25519", "open sesame");
    sshd.authorize(&env.ssh("id_locked.pub"));
    let (_, out) = env
        .connect("t", |k| match k {
            PromptKind::UnknownHostKey { .. } => PromptResponse::AcceptHostKey,
            PromptKind::KeyPassphrase { .. } => PromptResponse::Secret {
                value: SecretString::from("open sesame".to_owned()),
                remember: false,
            },
            other => panic!("{other:?}"),
        })
        .await;
    assert_connected(&out, &sshd);

    // 4. A key skipped after PK_OK (passphrase dismissed) must not break the
    //    protocol with OpenSSH: the next identity still authenticates.
    let mut env = Env::new(&host(
        "t",
        port,
        &user,
        "  IdentityFile ~/.ssh/id_locked\n  IdentityFile ~/.ssh/id_plain\n",
    ));
    keygen(&env.ssh("id_locked"), "ed25519", "secret");
    keygen(&env.ssh("id_plain"), "ed25519", "");
    sshd.authorize(&env.ssh("id_locked.pub"));
    sshd.authorize(&env.ssh("id_plain.pub"));
    let (_, out) = env
        .connect("t", |k| match k {
            PromptKind::UnknownHostKey { .. } => PromptResponse::AcceptHostKey,
            _ => PromptResponse::Cancel,
        })
        .await;
    assert_connected(&out, &sshd);
    assert!(
        out.prompts
            .iter()
            .any(|p| matches!(p, PromptKind::KeyPassphrase { .. }))
    );
    assert!(out.phases.contains(&SessionPhase::Authenticating {
        method: AuthMethod::PublicKey
    }));

    // 5. User certificate (TrustedUserCAKeys).
    let mut env = Env::new(&host("t", port, &user, "  IdentityFile ~/.ssh/id_cert\n"));
    keygen(&env.ssh("id_cert"), "ed25519", "");
    sign(&sshd.path("user_ca"), &env.ssh("id_cert.pub"), &user, false);
    let (_, out) = env.connect("t", accept_host).await;
    assert_connected(&out, &sshd);
    assert!(out.phases.contains(&SessionPhase::Authenticating {
        method: AuthMethod::Certificate
    }));

    // 6. Host certificate via @cert-authority: no TOFU prompt.
    let mut env = Env::new(&host(
        "t",
        port,
        &user,
        "  IdentityFile ~/.ssh/id_ed25519\n",
    ));
    keygen(&env.ssh("id_ed25519"), "ed25519", "");
    sshd.authorize(&env.ssh("id_ed25519.pub"));
    let ca = std::fs::read_to_string(sshd.path("host_ca.pub")).unwrap();
    env.append_known_hosts(&format!("@cert-authority [127.0.0.1]:{port} {}", ca.trim()));
    let (_, out) = env.connect("t", never).await;
    assert_connected(&out, &sshd);

    // 7. ProxyJump through sshd itself (direct-tcpip handled by OpenSSH).
    let mut env = Env::new(&format!(
        "IdentityFile ~/.ssh/id_ed25519\n{}{}",
        host("jump", port, &user, ""),
        host("inner", port, &user, "  ProxyJump jump\n")
    ));
    keygen(&env.ssh("id_ed25519"), "ed25519", "");
    sshd.authorize(&env.ssh("id_ed25519.pub"));
    let (_, out) = env.connect("inner", accept_host).await;
    assert_connected(&out, &sshd);
    assert!(out.phases.contains(&SessionPhase::Connecting {
        hop: 2,
        hops: 2,
        target: "inner".into()
    }));

    // 8. Real ssh-agent.
    let agent_dir = tempfile::tempdir().unwrap();
    let socket = agent_dir.path().join("agent.sock");
    let mut agent = Command::new("ssh-agent")
        .arg("-D")
        .arg("-a")
        .arg(&socket)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let key_dir = tempfile::tempdir().unwrap();
    let agent_key = key_dir.path().join("id_agent");
    keygen(&agent_key, "ed25519", "");
    sshd.authorize(&key_dir.path().join("id_agent.pub"));
    run(Command::new("ssh-add")
        .arg("-q")
        .arg(&agent_key)
        .env("SSH_AUTH_SOCK", &socket));
    let mut env = Env::with_agent(&host("t", port, &user, ""), socket);
    let (_, out) = env.connect("t", accept_host).await;
    let _ = agent.kill();
    assert_connected(&out, &sshd);
    assert!(out.phases.contains(&SessionPhase::Authenticating {
        method: AuthMethod::Agent
    }));
}
