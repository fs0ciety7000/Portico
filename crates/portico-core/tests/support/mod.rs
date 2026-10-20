//! In-process SSH server and engine harness (see `.claude/skills/ssh-testing`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    unreachable_pub
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use portico_core::engine::{ConnectRequest, PromptResponse};
use portico_core::events::{EngineEvent, PromptKind, SessionEvent, SessionPhase};
use portico_core::ids::SessionId;
use portico_core::secrets::MemoryStore;
use portico_core::store::Paths;
use portico_core::{Engine, EngineConfig};
use russh::Channel;
use russh::keys::ssh_key::LineEnding;
use russh::keys::ssh_key::certificate::{Builder, CertType};
use russh::keys::{Algorithm, Certificate, HashAlg, PrivateKey, PublicKey};
use russh::server::{Auth, Msg, Response, Session};
use tokio::net::TcpListener;
use tokio::sync::broadcast;

pub fn new_key() -> PrivateKey {
    PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ed25519).unwrap()
}

pub fn user_cert(ca: &PrivateKey, key: &PrivateKey, principal: &str, ty: CertType) -> Certificate {
    let mut b = Builder::new_with_random_nonce(
        &mut russh::keys::key::safe_rng(),
        key.public_key().key_data().clone(),
        0,
        u64::MAX,
    )
    .unwrap();
    b.cert_type(ty).unwrap();
    b.key_id(format!("{principal}-cert")).unwrap();
    b.valid_principal(principal).unwrap();
    b.sign(ca).unwrap()
}

/// What the test server accepts.
#[derive(Default)]
pub struct Behaviour {
    pub password: Option<(String, String)>,
    pub keys: Vec<PublicKey>,
    pub user_ca: Option<PublicKey>,
    pub keyboard_interactive: Option<(String, String)>,
    pub allow_direct_tcpip: bool,
    pub banner: Option<String>,
    /// keyboard-interactive asks a second question (PAM password + OTP).
    pub kbd_two_steps: bool,
    /// Counts accepted logins (for assertions).
    pub logins: AtomicUsize,
    /// Counts TCP connections accepted.
    pub connections: AtomicUsize,
    /// Counts publickey requests (probes included).
    pub pubkey_offers: AtomicUsize,
    /// Accept PTY shells that echo input; the line `exit` ends with status 3.
    pub pty_echo: bool,
    /// Last `window-change` received (cols, rows).
    pub last_resize: std::sync::Mutex<Option<(u32, u32)>>,
    /// `TERM` sent with the PTY request.
    pub term: std::sync::Mutex<Option<String>>,
}

pub struct TestServer {
    pub port: u16,
    pub host_key: PrivateKey,
    pub behaviour: Arc<Behaviour>,
}

impl TestServer {
    pub async fn start(behaviour: Behaviour) -> Self {
        Self::start_with(behaviour, new_key(), Vec::new()).await
    }

    pub async fn start_with(
        behaviour: Behaviour,
        host_key: PrivateKey,
        certificates: Vec<Certificate>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let behaviour = Arc::new(behaviour);
        let config = Arc::new(russh::server::Config {
            keys: vec![host_key.clone()],
            certificates,
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        let b = Arc::clone(&behaviour);
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                b.connections.fetch_add(1, Ordering::SeqCst);
                let handler = ServerHandler {
                    b: Arc::clone(&b),
                    kbd_step: 0,
                    kbd_first_ok: false,
                    channels: Vec::new(),
                    line: Vec::new(),
                };
                let config = Arc::clone(&config);
                tokio::spawn(async move {
                    if let Ok(running) = russh::server::run_stream(config, socket, handler).await {
                        let _ = running.await;
                    }
                });
            }
        });
        Self {
            port,
            host_key,
            behaviour,
        }
    }

    pub fn known_hosts_line(&self) -> String {
        format!(
            "[127.0.0.1]:{} {}",
            self.port,
            self.host_key.public_key().to_openssh().unwrap()
        )
    }
}

struct ServerHandler {
    b: Arc<Behaviour>,
    kbd_step: u8,
    kbd_first_ok: bool,
    /// Keeps accepted session channels alive.
    channels: Vec<Channel<Msg>>,
    /// Bytes received since the last newline (for the `exit` command).
    line: Vec<u8>,
}

impl russh::server::Handler for ServerHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(match &self.b.password {
            Some((u, p)) if u == user && p == password => self.accept(),
            _ => reject(),
        })
    }

    async fn auth_publickey_offered(
        &mut self,
        _user: &str,
        key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        self.b.pubkey_offers.fetch_add(1, Ordering::SeqCst);
        // Certificate probes carry the certified key: let them through to
        // `auth_openssh_certificate`, which checks the CA.
        Ok(if self.knows(key) || self.b.user_ca.is_some() {
            Auth::Accept
        } else {
            reject()
        })
    }

    async fn auth_publickey(&mut self, _user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        Ok(if self.knows(key) {
            self.accept()
        } else {
            reject()
        })
    }

    async fn auth_openssh_certificate(
        &mut self,
        user: &str,
        cert: &Certificate,
    ) -> Result<Auth, Self::Error> {
        let Some(ca) = &self.b.user_ca else {
            return Ok(reject());
        };
        let fp = ca.fingerprint(HashAlg::Sha256);
        let ok = cert.validate([&fp]).is_ok()
            && cert.cert_type() == CertType::User
            && cert.valid_principals().iter().any(|p| p == user);
        Ok(if ok { self.accept() } else { reject() })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        user: &str,
        _submethods: &str,
        response: Option<Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        let Some((u, p)) = &self.b.keyboard_interactive else {
            return Ok(reject());
        };
        let question = |q: &'static str| Auth::Partial {
            name: "".into(),
            instructions: "".into(),
            prompts: vec![(q.into(), false)].into(),
        };
        match response {
            None => {
                self.kbd_step = 0;
                Ok(question("Password: "))
            }
            Some(mut r) => {
                let answer = r.next().map(|b| String::from_utf8_lossy(&b).into_owned());
                let correct = user == u && answer.as_deref() == Some(p.as_str());
                if self.b.kbd_two_steps && self.kbd_step == 0 {
                    // PAM-like: keep asking even after a wrong first answer.
                    self.kbd_step = 1;
                    self.kbd_first_ok = correct;
                    return Ok(question("Verification code: "));
                }
                let ok = if self.b.kbd_two_steps {
                    self.kbd_first_ok
                } else {
                    correct
                };
                Ok(if ok { self.accept() } else { reject() })
            }
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.b.pty_echo {
            reply.accept().await;
            self.channels.push(channel);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)] // russh trait signature
    async fn pty_request(
        &mut self,
        channel: russh::ChannelId,
        term: &str,
        _cols: u32,
        _rows: u32,
        _pw: u32,
        _ph: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        *self.b.term.lock().unwrap() = Some(term.to_owned());
        session.channel_success(channel)
    }

    async fn shell_request(
        &mut self,
        channel: russh::ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.data(channel, bytes::Bytes::from_static(b"welcome\r\n"))
    }

    async fn window_change_request(
        &mut self,
        _channel: russh::ChannelId,
        cols: u32,
        rows: u32,
        _pw: u32,
        _ph: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        *self.b.last_resize.lock().unwrap() = Some((cols, rows));
        Ok(())
    }

    async fn data(
        &mut self,
        channel: russh::ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // Only PTY session channels echo; direct-tcpip channels carry other
        // traffic (a jump's inner SSH stream) that must pass through untouched.
        if !self.channels.iter().any(|c| c.id() == channel) {
            return Ok(());
        }
        session.data(channel, bytes::Bytes::copy_from_slice(data))?;
        for &b in data {
            if b == b'\r' || b == b'\n' {
                if self.line == b"exit" {
                    session.exit_status_request(channel, 3)?;
                    session.eof(channel)?;
                    session.close(channel)?;
                }
                self.line.clear();
            } else {
                self.line.push(b);
            }
        }
        Ok(())
    }

    async fn authentication_banner(&mut self) -> Result<Option<String>, Self::Error> {
        Ok(self.b.banner.clone())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if !self.b.allow_direct_tcpip {
            return Ok(()); // dropping `reply` rejects the channel
        }
        let target = format!("{host}:{port}");
        tokio::spawn(async move {
            let Ok(mut tcp) = tokio::net::TcpStream::connect(target).await else {
                return;
            };
            reply.accept().await;
            let mut stream = channel.into_stream();
            let _ = tokio::io::copy_bidirectional(&mut stream, &mut tcp).await;
        });
        Ok(())
    }
}

/// OpenSSH keeps every method available after a failure (russh's default
/// reject drops `password`); mirror OpenSSH so client retries are exercised.
fn reject() -> Auth {
    Auth::Reject {
        proceed_with_methods: Some(russh::MethodSet::server_supported()),
        partial_success: false,
    }
}

impl ServerHandler {
    fn knows(&self, key: &PublicKey) -> bool {
        self.b.keys.iter().any(|k| k.key_data() == key.key_data())
    }

    fn accept(&self) -> Auth {
        self.b.logins.fetch_add(1, Ordering::SeqCst);
        Auth::Accept
    }
}

/// Engine + temporary HOME.
pub struct Env {
    pub dir: tempfile::TempDir,
    pub engine: Engine,
    pub secrets: Arc<MemoryStore>,
    pub events: broadcast::Receiver<EngineEvent>,
}

/// Everything observed for one session until it settled.
#[derive(Debug, Default)]
pub struct Outcome {
    pub phases: Vec<SessionPhase>,
    pub prompts: Vec<PromptKind>,
    pub events: Vec<EngineEvent>,
}

impl Outcome {
    pub fn last(&self) -> &SessionPhase {
        self.phases.last().expect("at least one phase")
    }

    pub fn connected(&self) -> bool {
        matches!(self.last(), SessionPhase::Connected)
    }

    pub fn failure_code(&self) -> Option<&str> {
        match self.last() {
            SessionPhase::Failed { error } => Some(error.code.as_str()),
            _ => None,
        }
    }

    /// Everything the UIs could see, serialized: used for secret-leak checks.
    pub fn transcript(&self) -> String {
        serde_json::to_string(&self.events).unwrap()
    }
}

impl Env {
    pub fn new(ssh_config: &str) -> Self {
        Self::with_timeout(ssh_config, Duration::from_secs(10))
    }

    pub fn with_agent(ssh_config: &str, agent_socket: PathBuf) -> Self {
        Self::build(ssh_config, Duration::from_secs(10), Some(agent_socket))
    }

    pub fn with_timeout(ssh_config: &str, connect_timeout: Duration) -> Self {
        Self::build(ssh_config, connect_timeout, None)
    }

    fn build(ssh_config: &str, connect_timeout: Duration, agent_socket: Option<PathBuf>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ssh")).unwrap();
        std::fs::write(dir.path().join(".ssh/config"), ssh_config).unwrap();
        let secrets = Arc::new(MemoryStore::default());
        let engine = Engine::new(EngineConfig {
            paths: Paths::rooted(dir.path()),
            local_user: "alice".into(),
            event_capacity: 4096,
            secrets: Arc::clone(&secrets) as Arc<dyn portico_core::secrets::SecretStore>,
            agent_socket,
            connect_timeout,
            keepalive: Duration::from_millis(200),
        });
        let events = engine.subscribe();
        Self {
            dir,
            engine,
            secrets,
            events,
        }
    }

    pub fn ssh(&self, name: &str) -> PathBuf {
        self.dir.path().join(".ssh").join(name)
    }

    pub fn write_key(&self, name: &str, key: &PrivateKey, passphrase: Option<&str>) {
        let key = match passphrase {
            Some(p) => key.encrypt(&mut russh::keys::key::safe_rng(), p).unwrap(),
            None => key.clone(),
        };
        std::fs::write(
            self.ssh(name),
            key.to_openssh(LineEnding::LF).unwrap().as_bytes(),
        )
        .unwrap();
        std::fs::write(
            self.ssh(&format!("{name}.pub")),
            key.public_key().to_openssh().unwrap(),
        )
        .unwrap();
    }

    pub fn trust(&self, server: &TestServer) {
        self.append_known_hosts(&server.known_hosts_line());
    }

    pub fn append_known_hosts(&self, line: &str) {
        let path = self.ssh("known_hosts");
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        text.push_str(line);
        text.push('\n');
        std::fs::write(path, text).unwrap();
    }

    pub fn known_hosts(&self) -> String {
        std::fs::read_to_string(self.ssh("known_hosts")).unwrap_or_default()
    }

    pub async fn connect(
        &mut self,
        alias: &str,
        responder: impl FnMut(&PromptKind) -> PromptResponse,
    ) -> (SessionId, Outcome) {
        let id = self.engine.connect(ConnectRequest::new(alias)).unwrap();
        let outcome = self.settle(id, responder).await;
        (id, outcome)
    }

    /// Collects events for `id` until it is Connected, Failed or Closed,
    /// answering prompts with `responder`.
    pub async fn settle(
        &mut self,
        id: SessionId,
        mut responder: impl FnMut(&PromptKind) -> PromptResponse,
    ) -> Outcome {
        let mut out = Outcome::default();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(20), self.events.recv())
                .await
                .expect("session did not settle in time")
                .expect("event stream closed");
            out.events.push(event.clone());
            match event {
                EngineEvent::Prompt(p) if p.session == Some(id) => {
                    let answer = responder(&p.kind);
                    out.prompts.push(p.kind);
                    self.engine.respond(p.id, answer).unwrap();
                }
                EngineEvent::Session {
                    id: sid,
                    event: SessionEvent::Phase(phase),
                } if sid == id => {
                    let done = matches!(
                        phase,
                        SessionPhase::Connected
                            | SessionPhase::Failed { .. }
                            | SessionPhase::Closed { .. }
                    );
                    out.phases.push(phase);
                    if done {
                        return out;
                    }
                }
                _ => {}
            }
        }
    }

    /// Waits for a session event matching `pred`.
    pub async fn wait_for(
        &mut self,
        id: SessionId,
        mut pred: impl FnMut(&SessionEvent) -> bool,
    ) -> SessionEvent {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(20), self.events.recv())
                .await
                .expect("event did not arrive in time")
                .expect("event stream closed");
            if let EngineEvent::Session { id: sid, event } = event
                && sid == id
                && pred(&event)
            {
                return event;
            }
        }
    }
}

pub fn accept_host(kind: &PromptKind) -> PromptResponse {
    match kind {
        PromptKind::UnknownHostKey { .. } => PromptResponse::AcceptHostKey,
        other => panic!("unexpected prompt {other:?}"),
    }
}

pub fn never(kind: &PromptKind) -> PromptResponse {
    panic!("unexpected prompt {kind:?}")
}

pub fn target_config(alias: &str, port: u16, extra: &str) -> String {
    format!("Host {alias}\n  HostName 127.0.0.1\n  Port {port}\n  User alice\n{extra}")
}

/// Starts an in-process ssh-agent holding `keys`; returns its socket path.
#[cfg(unix)]
pub async fn start_agent(dir: &std::path::Path, keys: &[PrivateKey]) -> PathBuf {
    start_agent_with(dir, keys, false).await
}

/// An agent that lists its keys but refuses every signature (like a denied
/// `ssh-add -c` confirmation or a cancelled Touch ID prompt).
#[derive(Clone)]
struct RefusingAgent;

impl russh::keys::agent::server::Agent for RefusingAgent {
    async fn confirm_request(&self, msg: russh::keys::agent::server::MessageType) -> bool {
        !matches!(msg, russh::keys::agent::server::MessageType::Sign)
    }
}

#[cfg(unix)]
pub async fn start_agent_with(
    dir: &std::path::Path,
    keys: &[PrivateKey],
    refuse_signing: bool,
) -> PathBuf {
    let socket = dir.join("agent.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let incoming = futures::stream::unfold(listener, |l| async move {
        let next = l.accept().await.map(|(s, _)| s);
        Some((next, l))
    });
    if refuse_signing {
        tokio::spawn(russh::keys::agent::server::serve(
            Box::pin(incoming),
            RefusingAgent,
        ));
    } else {
        tokio::spawn(russh::keys::agent::server::serve(Box::pin(incoming), ()));
    }
    let mut client = russh::keys::agent::client::AgentClient::connect_uds(&socket)
        .await
        .unwrap();
    for key in keys {
        client.add_identity(key, &[]).await.unwrap();
    }
    socket
}
