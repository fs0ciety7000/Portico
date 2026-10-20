---
name: ssh-testing
description: Comment tester le moteur SSH de Portico sans réseau externe. Couvre le serveur russh en process, les fixtures de clés, de certificats et de known_hosts, les scénarios d'auth, ProxyJump, PTY, forwarding et SFTP. À charger avant d'écrire des tests de session, d'authentification, de tunnels ou de SFTP.
---

# Tester le protocole : Portico

## Principe

Aucun test ne dépend d'un `sshd` système ni d'Internet. On démarre un **serveur `russh::server` en process**
sur `127.0.0.1:0` (port éphémère) avec un `Handler` configurable, puis on pointe un `Paths::rooted(tempdir)`
contenant un `~/.ssh/config`, un `known_hosts` et des clés générés pour le test.

## Fixture (`crates/portico-core/tests/support/mod.rs`, en place)

```rust
pub struct TestServer {
    pub addr: SocketAddr,
    pub host_key: PrivateKey,          // ed25519 générée à la volée
    pub behaviour: Arc<Behaviour>,     // ce que le serveur accepte
    shutdown: CancellationToken,
}

pub struct Behaviour {
    pub accept_password: Option<(String, String)>,
    pub accept_keys: Vec<PublicKey>,
    pub trusted_ca: Option<PublicKey>,         // certificats
    pub keyboard_interactive: Option<Vec<(String, bool)>>,
    pub allow_direct_tcpip: bool,              // jump hosts, -L
    pub allow_tcpip_forward: bool,             // -R
    pub sftp_root: Option<TempDir>,            // sous-système SFTP (russh-sftp server)
    pub pty_echo: bool,                        // shell = echo des octets reçus
    pub delay_auth: Option<Duration>,          // tests de timeout
}

pub struct TestEnv { pub dir: TempDir, pub paths: Paths, pub engine: Engine }
impl TestEnv {
    pub fn with_config(cfg: &str) -> Self;            // écrit .ssh/config
    pub fn trust(&self, server: &TestServer);          // ajoute la ligne known_hosts
    pub fn client_key(&self, name: &str) -> PrivateKey; // écrit .ssh/<name>(.pub) en 0600
}
```

Les clés de test sont **générées** (`PrivateKey::random`) et jamais commitées. Pour les certificats, on crée une CA de test
et on signe avec `ssh_key::certificate::Builder`.

## Scénarios minimum (phase 1a)

| Scénario | Attendu |
|---|---|
| Hôte inconnu, réponse `AcceptHostKey` | `Prompt::UnknownHostKey`, puis `Connected`, et la ligne est ajoutée à `known_hosts` |
| Hôte inconnu, réponse `Cancel` | `Failed{code:"host_key_rejected"}`, `known_hosts` inchangé |
| `StrictHostKeyChecking yes` avec hôte inconnu | échec sans prompt |
| **Clé hôte modifiée** | `SessionEvent::HostKeyChanged`, puis `Failed{code:"host_key_changed"}`, **jamais** `Connected` |
| Auth par clé (sans passphrase, puis avec) | `Authenticating{PublicKey}`, puis prompt de passphrase, puis `Connected` |
| Auth par certificat signé par une CA de confiance | `Authenticating{Certificate}`, puis `Connected` |
| Certificat expiré | la méthode suivante est tentée, avec une ligne explicite dans le log |
| Mot de passe faux puis bon | deux prompts, puis `Connected` ; le mot de passe n'apparaît nulle part dans les logs ni les événements |
| ProxyJump à 2 hops (deux `TestServer`) | les phases `Connecting{hop:1,hops:2}` puis `{2,2}` |
| Cycle de jump (`A→B→A`) | `Failed{code:"proxy_jump_cycle"}` |
| Timeout de connexion (`delay_auth`) | `Failed{code:"timeout"}` sous `tokio::time::pause()` |
| Ordre des événements | séquence `Resolving → Connecting → Handshake → VerifyingHostKey → Authenticating → Connected` |

## Phases suivantes

- **PTY** : `pty_echo`, envoi de `b"hello\n"`, puis lecture sur `output`. Un resize doit être reçu par le handler du serveur.
- **Backpressure** : un serveur qui envoie 50 Mo face à un consommateur lent. La mémoire reste bornée (capacité du canal) et rien n'est perdu.
- **-L** : on lance un echo TCP local « distant », on ouvre le forward puis on vérifie le round-trip. Un port occupé produit `TunnelEvent::Failed`.
- **-R** : le serveur ouvre `forwarded-tcpip`, et la connexion locale doit aboutir.
- **SFTP** : `russh-sftp` côté serveur sur `sftp_root`. On teste list, upload et download 10 Mo (checksum), l'annulation (pas de fichier partiel final), `.part` puis `rename`.
- **Copy-id** : un serveur avec exec simulé sur un `authorized_keys` temporaire. Deux appels ne doivent produire **qu'une seule** ligne, et les permissions doivent être vérifiées.

## Interop OpenSSH réel

`crates/portico-core/tests/openssh_interop.rs` lance un vrai `sshd` (non-root possible) avec `ssh-keygen` et `ssh-agent` :
ed25519, RSA (rsa-sha2), clé chiffrée, clé ignorée après PK_OK, certificat utilisateur, certificat d'hôte, ProxyJump, agent.
Le test est `#[ignore]` par défaut. Pour le lancer : `cargo test -p portico-core --test openssh_interop -- --ignored` (job CI dédié).
**Toute modification du transport ou de l'auth doit repasser ce test.**

## Hygiène

- Chaque test a son `TempDir` et son `CancellationToken`, et s'arrête proprement (pas de tâche orpheline).
- Les timeouts de test sont courts mais déterministes : on préfère `tokio::time::pause()` à de vraies attentes.
- Pour vérifier qu'aucun secret ne fuit, on capture les `EngineEvent` et les `LogRecord` d'un test d'auth et on vérifie que le mot de passe n'y figure pas.
