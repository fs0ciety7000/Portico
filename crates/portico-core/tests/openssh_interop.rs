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
use portico_core::events::{
    AuthMethod, EngineEvent, PromptKind, SessionPhase, SftpEvent, TunnelEvent,
};
use portico_core::forward::{Endpoint, ForwardSpec};
use portico_core::sftp::{Direction, OnConflict, TransferRequest};
use secrecy::SecretString;
use support::{Env, accept_host, never};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
        // Sessions get a throwaway HOME, so copy-id (scenario 12) writes
        // there and never in the real ~/.ssh of whoever runs the tests.
        std::fs::create_dir_all(d.join("home")).unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let config = format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {d}/host_ed25519\n\
             HostCertificate {d}/host_ed25519-cert.pub\nPidFile {d}/sshd.pid\n\
             AuthorizedKeysFile {d}/authorized_keys {d}/home/.ssh/authorized_keys\nSetEnv HOME={d}/home\n\
             TrustedUserCAKeys {d}/user_ca.pub\n\
             StrictModes no\nUsePAM no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n\
             PermitRootLogin yes\nAllowTcpForwarding yes\nSubsystem sftp internal-sftp\nLogLevel DEBUG\n",
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

/// Collects the progress of `t` until it ends; `cancel_at` cancels it once
/// that many bytes moved. Returns the progress values and how it ended.
async fn follow(
    env: &mut Env,
    t: portico_core::ids::TransferId,
    cancel_at: Option<u64>,
) -> (Vec<u64>, &'static str) {
    let mut seen = Vec::new();
    loop {
        let event = match env.events.recv().await {
            Ok(EngineEvent::Sftp { event, .. }) => event,
            // Progress bursts may overflow the test's receiver: keep going.
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(e) => panic!("{e}"),
        };
        match event {
            SftpEvent::TransferProgress { transfer, done, .. } if transfer == t => {
                seen.push(done);
                if cancel_at.is_some_and(|at| done >= at) {
                    env.engine.cancel_transfer(t).unwrap();
                }
            }
            SftpEvent::TransferFinished { transfer, .. } if transfer == t => {
                return (seen, "finished");
            }
            SftpEvent::TransferCancelled { transfer } if transfer == t => {
                return (seen, "cancelled");
            }
            SftpEvent::TransferFailed { transfer, error } if transfer == t => panic!("{error:?}"),
            _ => {}
        }
    }
}

// Fixed-size buffers sliced by what `read` returned.
#[allow(clippy::indexing_slicing, clippy::many_single_char_names)]
fn same_content(a: &Path, b: &Path) -> bool {
    use std::io::Read;
    let (mut a, mut b) = (
        std::fs::File::open(a).unwrap(),
        std::fs::File::open(b).unwrap(),
    );
    let (mut x, mut y) = (vec![0_u8; 1 << 20], vec![0_u8; 1 << 20]);
    loop {
        let n = a.read(&mut x).unwrap();
        if n == 0 {
            return b.read(&mut y).unwrap() == 0;
        }
        b.read_exact(&mut y[..n]).unwrap();
        if x[..n] != y[..n] {
            return false;
        }
    }
}

// Fixed-size blocks; the throughput line is the report of this opt-in run.
#[allow(clippy::indexing_slicing, clippy::print_stderr)]
async fn big_transfers(
    env: &mut Env,
    id: portico_core::ids::SessionId,
    local: &Path,
    remote: &str,
) {
    use std::io::Write;
    const GIB: u64 = 1 << 30;
    let src = local.join("big.bin");
    {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
        let block: Vec<u8> = (0..(1_u32 << 20)).map(|i| (i % 253) as u8).collect();
        for i in 0..(GIB >> 20) {
            let mut b = block.clone();
            b[..8].copy_from_slice(&i.to_le_bytes());
            f.write_all(&b).unwrap();
        }
    }
    for (direction, from, to) in [
        (Direction::Upload, src.clone(), format!("{remote}/big.bin")),
        (
            Direction::Download,
            local.join("big-back.bin"),
            format!("{remote}/big.bin"),
        ),
    ] {
        let started = std::time::Instant::now();
        let t = env
            .engine
            .transfer(
                id,
                TransferRequest {
                    direction,
                    local: from,
                    remote: to,
                    on_conflict: OnConflict::Ask,
                },
            )
            .await
            .unwrap();
        let (seen, end) = follow(env, t, None).await;
        assert_eq!(end, "finished");
        assert!(
            seen.windows(2).all(|w| w[0] <= w[1]),
            "progress goes forward"
        );
        assert!(seen.len() > 10, "progress reported along the way");
        let secs = started.elapsed().as_secs_f64();
        eprintln!(
            "1 GiB {direction:?}: {secs:.1} s ({:.0} MiB/s), {} progress events",
            1024.0 / secs,
            seen.len()
        );
    }
    assert!(same_content(&src, &local.join("big-back.bin")));
    // Cancel midway: no destination and no partial file left behind.
    let t = env
        .engine
        .transfer(
            id,
            TransferRequest {
                direction: Direction::Download,
                local: local.join("cancelled.bin"),
                remote: format!("{remote}/big.bin"),
                on_conflict: OnConflict::Ask,
            },
        )
        .await
        .unwrap();
    let (_, end) = follow(env, t, Some(GIB / 4)).await;
    assert_eq!(end, "cancelled");
    let leftovers: Vec<_> = std::fs::read_dir(local)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("cancelled"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let t = env
        .engine
        .transfer(
            id,
            TransferRequest {
                direction: Direction::Upload,
                local: src.clone(),
                remote: format!("{remote}/cancelled.bin"),
                on_conflict: OnConflict::Ask,
            },
        )
        .await
        .unwrap();
    let (_, end) = follow(env, t, Some(GIB / 4)).await;
    assert_eq!(end, "cancelled");
    let remote_left: Vec<_> = std::fs::read_dir(remote)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("cancelled"))
        .collect();
    assert!(remote_left.is_empty(), "{remote_left:?}");
    for f in ["big.bin", "big-back.bin"] {
        let _ = std::fs::remove_file(local.join(f));
    }
    let _ = std::fs::remove_file(format!("{remote}/big.bin"));
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

    // 9. SFTP (internal-sftp): list, upload, download, rename, remove.
    // Real keepalive budget (the test default is 200 ms), for scenario 11.
    let mut env = Env::new(&host(
        "t",
        port,
        &user,
        "  IdentityFile ~/.ssh/id_ed25519\n  ServerAliveInterval 2\n",
    ));
    keygen(&env.ssh("id_ed25519"), "ed25519", "");
    sshd.authorize(&env.ssh("id_ed25519.pub"));
    let (id, out) = env.connect("t", accept_host).await;
    assert_connected(&out, &sshd);
    let remote_dir = tempfile::tempdir().unwrap();
    let remote_dir_s = remote_dir.path().display().to_string();
    std::fs::write(remote_dir.path().join("hello.txt"), b"hello").unwrap();
    let list = env.engine.sftp_list(id, &remote_dir_s).await.unwrap();
    assert_eq!(list.len(), 1);
    assert!(!env.engine.sftp_home(id).await.unwrap().is_empty());
    let local = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..3_000_000_u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(local.path().join("up.bin"), &data).unwrap();
    for request in [
        TransferRequest {
            direction: Direction::Upload,
            local: local.path().join("up.bin"),
            remote: format!("{remote_dir_s}/up.bin"),
            on_conflict: OnConflict::Ask,
        },
        TransferRequest {
            direction: Direction::Download,
            local: local.path().join("down.bin"),
            remote: format!("{remote_dir_s}/up.bin"),
            on_conflict: OnConflict::Ask,
        },
    ] {
        let t = env.engine.transfer(id, request).await.unwrap();
        loop {
            match env.events.recv().await.unwrap() {
                EngineEvent::Sftp {
                    event: SftpEvent::TransferFinished { transfer, .. },
                    ..
                } if transfer == t => break,
                EngineEvent::Sftp {
                    event: SftpEvent::TransferFailed { transfer, error },
                    ..
                } if transfer == t => {
                    panic!("{error:?}")
                }
                _ => {}
            }
        }
    }
    assert_eq!(std::fs::read(local.path().join("down.bin")).unwrap(), data);
    // Phase 2 exit criterion, opt-in (disk and time): PORTICO_INTEROP_BIG=1
    // moves 1 GiB each way with progress, then cancels a transfer midway.
    if std::env::var_os("PORTICO_INTEROP_BIG").is_some() {
        big_transfers(&mut env, id, local.path(), &remote_dir_s).await;
    }
    env.engine
        .sftp_rename(
            id,
            &format!("{remote_dir_s}/up.bin"),
            &format!("{remote_dir_s}/moved.bin"),
        )
        .await
        .unwrap();
    env.engine
        .sftp_remove(id, &format!("{remote_dir_s}/moved.bin"))
        .await
        .unwrap();

    // 10. -L and -R through OpenSSH to a local echo service.
    let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_port = echo.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = echo.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    let ep = |h: &str, p: u16| Endpoint {
        host: h.into(),
        port: p,
    };
    for spec in [
        ForwardSpec::Local {
            bind: ep("127.0.0.1", 0),
            target: ep("127.0.0.1", echo_port),
        },
        ForwardSpec::Remote {
            bind: ep("127.0.0.1", 0),
            target: ep("127.0.0.1", echo_port),
        },
    ] {
        let tunnel = env
            .engine
            .start_tunnel(id, spec.clone(), false)
            .await
            .unwrap();
        let bind = loop {
            match env.events.recv().await.unwrap() {
                EngineEvent::Tunnel {
                    id: t,
                    event: TunnelEvent::Listening { bind },
                    ..
                } if t == tunnel => break bind,
                EngineEvent::Tunnel {
                    id: t,
                    event: TunnelEvent::Failed { error },
                    ..
                } if t == tunnel => {
                    panic!("{spec}: {error:?}\n{}", sshd.log())
                }
                _ => {}
            }
        };
        let p: u16 = bind
            .split(", ")
            .next()
            .unwrap()
            .rsplit(':')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", p))
            .await
            .unwrap();
        s.write_all(b"via openssh").await.unwrap();
        let mut buf = [0_u8; 11];
        tokio::time::timeout(Duration::from_secs(5), s.read_exact(&mut buf))
            .await
            .expect("echo through the tunnel")
            .unwrap();
        assert_eq!(&buf, b"via openssh", "{spec}");
        env.engine.stop_tunnel(tunnel).unwrap();
    }

    // 11. A local peer that stops reading a busy tunnel is cut (russh has one
    // loop per session), and the session keeps serving other tunnels.
    let flood = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let flood_port = flood.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = flood.accept().await {
            tokio::spawn(async move {
                let block = vec![b'x'; 64 * 1024];
                while s.write_all(&block).await.is_ok() {}
            });
        }
    });
    let mut local_tunnel = async |target: u16| {
        let tunnel = env
            .engine
            .start_tunnel(
                id,
                ForwardSpec::Local {
                    bind: ep("127.0.0.1", 0),
                    target: ep("127.0.0.1", target),
                },
                false,
            )
            .await
            .unwrap();
        loop {
            if let EngineEvent::Tunnel {
                id: t,
                event: TunnelEvent::Listening { bind },
                ..
            } = env.events.recv().await.unwrap()
                && t == tunnel
            {
                break bind.rsplit(':').next().unwrap().parse::<u16>().unwrap();
            }
        }
    };
    let busy = local_tunnel(flood_port).await;
    let _hog = tokio::net::TcpStream::connect(("127.0.0.1", busy))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(4)).await;
    let other = local_tunnel(echo_port).await;
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", other))
        .await
        .unwrap();
    s.write_all(b"still alive").await.unwrap();
    let mut buf = [0_u8; 11];
    tokio::time::timeout(Duration::from_secs(5), s.read_exact(&mut buf))
        .await
        .expect("other tunnels keep working after a stalled peer")
        .unwrap();
    assert_eq!(&buf, b"still alive");
    let logs = env.engine.session_logs(id).unwrap();
    assert!(
        logs.iter().any(|l| l.message.contains("stopped reading")),
        "the stall guard should have fired"
    );

    // 12. Phase 3: generate a key in Portico, install it with copy-id over
    // an authenticated session, log in with it alone, manage it in a real
    // ssh-agent.
    copy_id_and_agent(&sshd, port, &user).await;

    // 13. Phase 5: -D (SOCKS5) through OpenSSH on the session of scenario 9:
    // a SOCKS5 client written here asks for a name and an IPv4 address (with
    // pipelined data) and for a closed port, then curl, when installed,
    // fetches a page from a local HTTP server and is refused as SOCKS4a.
    socks_through_openssh(&mut env, id, echo_port).await;
}

/// SOCKS5 CONNECT to `host` (IPv4 address → ATYP 1, else a domain name →
/// ATYP 3), greeting, request and `early` data sent in one write; returns
/// the reply code and the stream.
async fn socks5_connect_with(
    proxy: u16,
    host: &str,
    port: u16,
    early: &[u8],
) -> (u8, tokio::net::TcpStream) {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", proxy))
        .await
        .unwrap();
    let mut hello = vec![5, 1, 0, 5, 1, 0];
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        hello.push(1);
        hello.extend(ip.octets());
    } else {
        hello.extend([3, u8::try_from(host.len()).unwrap()]);
        hello.extend(host.as_bytes());
    }
    hello.extend(port.to_be_bytes());
    hello.extend(early);
    // Greeting, request (and data) pipelined, as curl does not but some
    // clients do.
    s.write_all(&hello).await.unwrap();
    let mut answer = [0_u8; 12];
    tokio::time::timeout(Duration::from_secs(10), s.read_exact(&mut answer))
        .await
        .expect("SOCKS reply in time")
        .unwrap();
    assert_eq!(answer[..2], [5, 0], "no authentication chosen");
    (answer[3], s)
}

async fn socks5_connect(proxy: u16, host: &str, port: u16) -> (u8, tokio::net::TcpStream) {
    socks5_connect_with(proxy, host, port, b"").await
}

/// Scenario 13. What sshd does with SOCKS destinations: a name, an IPv4
/// address, data pipelined with the handshake, a closed port; curl both
/// ways (SOCKS5 with remote names, SOCKS 4a refused). That names are never
/// resolved locally is proven by the in-process tests (the fixture records
/// what reached the server); here sshd resolving `localhost` itself is
/// only consistent with it.
async fn socks_through_openssh(env: &mut Env, id: portico_core::ids::SessionId, echo_port: u16) {
    let spec = ForwardSpec::Dynamic {
        bind: Endpoint {
            host: "127.0.0.1".into(),
            port: 0,
        },
    };
    let tunnel = env.engine.start_tunnel(id, spec, false).await.unwrap();
    let proxy = loop {
        match env.events.recv().await.unwrap() {
            EngineEvent::Tunnel {
                id: t,
                event: TunnelEvent::Listening { bind },
                ..
            } if t == tunnel => break bind.rsplit(':').next().unwrap().parse::<u16>().unwrap(),
            EngineEvent::Tunnel {
                id: t,
                event: TunnelEvent::Failed { error },
                ..
            } if t == tunnel => panic!("-D: {error:?}"),
            _ => {}
        }
    };
    let (code, mut s) = socks5_connect(proxy, "localhost", echo_port).await;
    assert_eq!(code, 0, "CONNECT localhost through sshd");
    s.write_all(b"via socks").await.unwrap();
    let mut buf = [0_u8; 9];
    tokio::time::timeout(Duration::from_secs(5), s.read_exact(&mut buf))
        .await
        .expect("echo through SOCKS")
        .unwrap();
    assert_eq!(&buf, b"via socks");
    drop(s);

    // ATYP 1 (IPv4), with data sent before the reply in the same write.
    let (code, mut s) = socks5_connect_with(proxy, "127.0.0.1", echo_port, b"ping").await;
    assert_eq!(code, 0, "CONNECT 127.0.0.1 through sshd");
    let mut buf = [0_u8; 4];
    tokio::time::timeout(Duration::from_secs(5), s.read_exact(&mut buf))
        .await
        .expect("pipelined data echoed")
        .unwrap();
    assert_eq!(&buf, b"ping");
    drop(s);

    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let (code, _) = socks5_connect(proxy, "127.0.0.1", closed).await;
    assert_eq!(code, 0x05, "sshd's connect failure → connection refused");
    let (code, _) = socks5_connect(proxy, "localhost", closed).await;
    assert_eq!(code, 0x05, "by name too");
    let logs = env.engine.session_logs(id).unwrap();
    for expected in [
        format!("→ 127.0.0.1:{closed}: refused or unreachable from the server"),
        format!("→ localhost:{closed}: refused or unresolvable on the server"),
    ] {
        assert!(
            logs.iter().any(|l| l.message.ends_with(&expected)),
            "{expected}"
        );
    }

    // curl --socks5-hostname: the URL's host name goes to sshd unresolved
    // (skipped without curl: the client above covers the protocol).
    let curl_present = Command::new("curl")
        .arg("--version")
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if curl_present {
        let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_port = http.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = http.accept().await {
                tokio::spawn(async move {
                    let mut request = [0_u8; 1024];
                    let _ = s.read(&mut request).await;
                    let _ = s
                        .write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 13\r\n\r\nportico-socks")
                        .await;
                });
            }
        });
        let curl = |flag: &'static str| {
            let url = format!("http://localhost:{http_port}/");
            let socks = format!("127.0.0.1:{proxy}");
            tokio::task::spawn_blocking(move || {
                let mut curl = Command::new("curl");
                for var in ["http_proxy", "https_proxy", "all_proxy", "no_proxy"] {
                    curl.env_remove(var).env_remove(var.to_uppercase());
                }
                curl.args(["-sS", "--max-time", "10", "--noproxy", ""])
                    .args([flag, &socks, &url])
                    .output()
                    .unwrap()
            })
        };
        let out = curl("--socks5-hostname").await.unwrap();
        assert!(
            out.status.success(),
            "curl: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.stdout, b"portico-socks");
        // SOCKS4a is refused with its own reply, at once: curl reports a
        // proxy error (CURLE_PROXY, 97), not a timeout.
        let started = std::time::Instant::now();
        let out = curl("--socks4a").await.unwrap();
        let elapsed = started.elapsed();
        let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
        assert!(out.stdout.is_empty());
        assert!(
            out.status.code() == Some(97)
                || (stderr.contains("reject") && !stderr.contains("timed out")),
            "SOCKS4a must be refused: {:?} {stderr}",
            out.status.code()
        );
        assert!(elapsed < Duration::from_secs(2), "refused in {elapsed:?}");
        let logs = env.engine.session_logs(id).unwrap();
        assert!(
            logs.iter()
                .any(|l| l.message.contains("SOCKS4 client refused")),
            "Portico logged the SOCKS4 refusal"
        );
    }
    env.engine.stop_tunnel(tunnel).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let EngineEvent::Tunnel {
                id: t,
                event: TunnelEvent::Stopped,
                ..
            } = env.events.recv().await.unwrap()
                && t == tunnel
            {
                break;
            }
        }
    })
    .await
    .expect("-D stopped");
}

async fn copy_id_and_agent(sshd: &Sshd, port: u16, user: &str) {
    use portico_core::keys::{KeyAlgorithm, KeyGenRequest};
    let mut env = Env::new(&host("t", port, user, "  IdentityFile ~/.ssh/id_ed25519\n"));
    keygen(&env.ssh("id_ed25519"), "ed25519", "");
    sshd.authorize(&env.ssh("id_ed25519.pub"));
    let (id, out) = env.connect("t", accept_host).await;
    assert_connected(&out, sshd);

    let generated = env
        .engine
        .generate_key(KeyGenRequest {
            name: "id_portico".into(),
            algorithm: KeyAlgorithm::Ed25519,
            comment: "portico@test".into(),
            passphrase: Some(SecretString::from("pp-1234".to_owned())),
        })
        .await
        .unwrap();
    // OpenSSH reads what Portico wrote.
    let derived = Command::new("ssh-keygen")
        .args(["-y", "-P", "pp-1234", "-f"])
        .arg(&generated.private_path)
        .output()
        .unwrap();
    assert!(
        derived.status.success(),
        "{}",
        String::from_utf8_lossy(&derived.stderr)
    );
    let pub_text = std::fs::read_to_string(&generated.public_path).unwrap();
    let fields = |t: &str| t.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
    assert_eq!(
        fields(&pub_text),
        fields(&String::from_utf8_lossy(&derived.stdout))
    );

    // A login shell rc that talks (very common): copy-id must not care.
    std::fs::write(sshd.path("home/.bashrc"), "echo hello from bashrc\n").unwrap();
    let installed = sshd.path("home/.ssh/authorized_keys");
    assert_eq!(
        env.engine
            .copy_id(id, generated.public_path.clone())
            .await
            .unwrap(),
        portico_core::CopyIdOutcome::Added
    );
    assert_eq!(
        env.engine
            .copy_id(id, generated.public_path.clone())
            .await
            .unwrap(),
        portico_core::CopyIdOutcome::AlreadyPresent
    );
    let lines = std::fs::read_to_string(&installed).unwrap();
    assert_eq!(lines.lines().count(), 1, "{lines}");
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&installed).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    // Log in with the new key only; its passphrase is asked for.
    let log_before = sshd.log().len();
    let served = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let answers = tokio::spawn(answer_passphrases(
        env.engine.clone(),
        "pp-1234",
        std::sync::Arc::clone(&served),
    ));
    env.engine
        .verify_key_login(id, generated.private_path.clone())
        .await
        .unwrap();
    answers.abort();
    assert!(served.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    let log = sshd.log();
    let new_lines = log.get(log_before..).unwrap_or_default();
    assert!(
        new_lines.contains("Accepted publickey") && new_lines.contains(&generated.fingerprint),
        "{new_lines}"
    );

    // A key that is not installed is reported as such.
    let stranger = env
        .engine
        .generate_key(KeyGenRequest {
            name: "id_stranger".into(),
            algorithm: KeyAlgorithm::Ecdsa {
                curve: portico_core::keys::Curve::P256,
            },
            comment: String::new(),
            passphrase: None,
        })
        .await
        .unwrap();
    let err = env
        .engine
        .verify_key_login(id, stranger.private_path.clone())
        .await
        .unwrap_err();
    assert_eq!(err.summary().code, "copy_id_not_accepted");

    // Even with a certificate the server trusts next to it: the proof is
    // about authorized_keys, so the certificate must not be used.
    sign(&sshd.path("user_ca"), &stranger.public_path, user, false);
    assert!(env.ssh("id_stranger-cert.pub").exists());
    let err = env
        .engine
        .verify_key_login(id, stranger.private_path.clone())
        .await
        .unwrap_err();
    assert_eq!(err.summary().code, "copy_id_not_accepted");

    // ssh-agent: add (with passphrase and lifetime), list, remove.
    let agent_dir = tempfile::tempdir().unwrap();
    let socket = agent_dir.path().join("agent.sock");
    let _agent = Killed(
        Command::new("ssh-agent")
            .arg("-D")
            .arg("-a")
            .arg(&socket)
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let env = Env::with_agent(&host("t", port, user, ""), socket.clone());
    assert!(env.engine.agent_keys().await.unwrap().is_empty());
    let err = env
        .engine
        .agent_add(generated.private_path.clone(), None, None)
        .await
        .unwrap_err();
    assert_eq!(err.summary().code, "key_needs_passphrase");
    let added = env
        .engine
        .agent_add(
            generated.private_path.clone(),
            Some(SecretString::from("pp-1234".to_owned())),
            Some(Duration::from_secs(600)),
        )
        .await
        .unwrap();
    assert_eq!(added.fingerprint, generated.fingerprint);
    let listed = env.engine.agent_keys().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed.first().unwrap().fingerprint, generated.fingerprint);
    let ssh_add = Command::new("ssh-add")
        .arg("-l")
        .env("SSH_AUTH_SOCK", &socket)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&ssh_add.stdout).contains(&generated.fingerprint));
    env.engine
        .agent_remove(&generated.fingerprint)
        .await
        .unwrap();
    assert!(env.engine.agent_keys().await.unwrap().is_empty());
    assert_eq!(
        env.engine
            .agent_remove(&generated.fingerprint)
            .await
            .unwrap_err()
            .summary()
            .code,
        "agent_no_identity"
    );
    // A lifetime is enforced by the agent.
    env.engine
        .agent_add(
            stranger.private_path.clone(),
            None,
            Some(Duration::from_secs(1)),
        )
        .await
        .unwrap();
    assert_eq!(env.engine.agent_keys().await.unwrap().len(), 1);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(env.engine.agent_keys().await.unwrap().is_empty());
}

/// Kills a helper process however the test ends.
struct Killed(std::process::Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Answers every key passphrase prompt of `engine` with `passphrase`.
async fn answer_passphrases(
    engine: portico_core::Engine,
    passphrase: &'static str,
    served: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    let mut events = engine.subscribe();
    loop {
        match events.recv().await {
            Ok(EngineEvent::Prompt(prompt)) => {
                if let PromptKind::KeyPassphrase { .. } = prompt.kind {
                    served.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let _ = engine.respond(
                        prompt.id,
                        PromptResponse::Secret {
                            value: SecretString::from(passphrase.to_owned()),
                            remember: false,
                        },
                    );
                }
            }
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(_) => return,
        }
    }
}
