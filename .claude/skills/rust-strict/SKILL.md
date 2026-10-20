---
name: rust-strict
description: Rust sécurisé et idiomatique pour Portico. Couvre les erreurs typées, l'interdiction des panics, l'architecture événementielle du moteur (sessions, PTY, tunnels, SFTP, snippets, génération et copie de clés) et les patterns async tokio. À charger avant d'écrire ou de relire du Rust, en particulier dans crates/portico-core.
---

# Rust strict : Portico

## 1. Non négociables (vérifiés par les lints du workspace)

- Hors `#[cfg(test)]`, **pas** de `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, `dbg!`, ni de `unsafe`.
- `cargo clippy --all-targets` doit passer **sans warning** (le niveau `pedantic` est actif). Un `#[allow]` est local et **commenté**.
- L'indexation passe par `.get()`, `.first()` ou par pattern matching (lint `indexing_slicing`).
- Un `Mutex` empoisonné se gère avec `lock().unwrap_or_else(PoisonError::into_inner)`.
- Les types publics implémentent `Debug`, et les types qui contiennent un secret ont un `Debug` caviardé.

## 2. Erreurs

```rust
// Un enum par module, précis, avec le contexte nécessaire au diagnostic.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    #[error("no authentication method accepted by {host} (tried: {tried})")]
    Exhausted { host: String, tried: String },
    #[error("cannot decrypt key {path}: wrong passphrase")]
    BadPassphrase { path: PathBuf },
}
// Agrégation dans crate::Error via #[from], et ajout d'un code stable dans Error::summary().
```

- Dans `portico-core`, on n'utilise ni `anyhow` ni `Box<dyn Error>` dans les API publiques.
- Le message est en minuscules, contient le contexte (hôte, chemin, ligne), et **jamais** de secret ni de contenu de terminal.
- Toute nouvelle variante reçoit un `code` et un `hint` actionnable dans `Error::summary()`. L'UI n'affiche que `ErrorSummary`.
- On convertit à la frontière (`map_err`), sans laisser fuir les types d'erreur de russh ou de keyring dans l'API publique.

## 3. Secrets

- Ils se transportent en `secrecy::SecretString` ou `SecretBox<[u8]>`. `expose_secret()` n'est appelé qu'au point d'usage (au plus près de russh ou du keyring).
- Les buffers temporaires sont des `zeroize::Zeroizing<Vec<u8>>`.
- Un secret ne finit jamais dans un `EngineEvent`, un `LogRecord`, un champ `tracing` (`%`/`?`) ou un message d'erreur.
- Pas de `Clone` superflu sur un secret, et pas de `String` intermédiaire.

## 4. Architecture événementielle du moteur

```
UI ──commande async──▶ Engine ──spawn──▶ SessionActor (possède russh Handle)
 ▲                        │                  ├─ TerminalTask (canal PTY)
 │  broadcast<EngineEvent>│                  ├─ SftpTask
 └────────────────────────┘                  ├─ ForwardTask (-L / -R)
 ▲  mpsc bornés (data plane)                  └─ KeepaliveTask (latence)
 └──────── TerminalHandle { input, output } ◀─┘
```

Règles :

1. **Plan de contrôle** (`broadcast<EngineEvent>`) : transitions d'état, prompts, progression, logs. Les événements sont petits et `Clone`.
2. **Plan de données** (`mpsc` **borné**) : octets PTY et blocs de transfert. La capacité est une constante nommée (`TERMINAL_OUTPUT_CAPACITY`).
   L'acteur fait `send().await`, ce qui propage la backpressure jusqu'à la fenêtre SSH. On n'utilise **jamais** d'`unbounded` pour les données distantes.
3. **Une transition = un événement** : chaque changement de `SessionPhase` est émis *avant* l'action suivante, ce qui rend l'UI fidèle.
4. **Prompts** : `let rx = engine.register_prompt(id); engine.emit(Prompt{..}); select! { r = rx => …, _ = cancel.cancelled() => … }`.
   Seule l'opération attend. Il faut toujours prévoir l'annulation et un `PromptResolved` en sortie.
5. **Annulation** : chaque acteur reçoit un `CancellationToken` enfant de celui de la session, lui-même enfant de l'engine.
   Fermer la session annule tout l'arbre. On utilise `tokio::select!` avec `biased;` quand l'ordre compte (annulation d'abord).
6. **Pas d'état partagé mutable entre acteurs** : on communique par messages. Un `Mutex` n'est permis que pour des registres courts
   (prompts en attente, table des sessions), jamais tenu à travers un `.await`. On utilise `std::sync::Mutex` pour ces sections courtes.
7. **Bloquant** : fichiers, keyring et génération RSA passent par `tokio::task::spawn_blocking`. Pas de `std::fs` dans une tâche async, sauf dans `spawn_blocking`.
8. **Timeouts partout** sur le réseau : `tokio::time::timeout(connect_timeout, …)`. Valeurs par défaut : connect 15 s, auth 30 s.

### Pattern acteur de session (gabarit)

```rust
pub(crate) struct SessionActor {
    id: SessionId,
    engine: Engine,               // pour emit() et les prompts
    cancel: CancellationToken,
    commands: mpsc::Receiver<SessionCommand>,
}

impl SessionActor {
    pub(crate) async fn run(mut self) {
        let result = self.connect().await;           // émet Resolving → … → Connected
        let handle = match result {
            Ok(h) => h,
            Err(e) => return self.phase(SessionPhase::Failed { error: e.summary() }),
        };
        loop {
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => break,
                Some(cmd) = self.commands.recv() => self.handle(cmd, &handle).await,
                else => break,
            }
        }
        self.phase(SessionPhase::Closed { reason: CloseReason::UserRequested });
    }
}
```

## 5. Domaines spécifiques

- **Snippets** : on envoie le corps rendu sur le `TerminalHandle.input` actif, sans jamais l'exécuter localement.
  Les placeholders sont résolus par l'UI avant l'envoi.
- **Génération de clés** : `PrivateKey::random(&mut rng, Algorithm)` dans `spawn_blocking`, avec une RNG système (`getrandom`).
  RSA doit faire au moins 3072 bits. On chiffre si une passphrase est fournie. On écrit via `store::write_atomic` (`0600`) et on
  **refuse d'écraser** un fichier existant (`create_new`). Le `.pub` est en `0644`.
- **Copy-id** : une session authentifiée exécute un script POSIX **idempotent**
  (`umask 077; mkdir -p ~/.ssh; touch ~/.ssh/authorized_keys; grep -qxF "$KEY" … || printf '%s\n' "$KEY" >> …`).
  La clé publique est passée par **stdin du canal exec**, jamais interpolée dans la ligne de commande. On vérifie ensuite par reconnexion.
- **Tunnels** : bind `localhost` par défaut. Une erreur de bind est un `TunnelEvent::Failed` explicite (port occupé → hint).
- **SFTP** : transferts en chunks (256 Kio) avec une progression émise au plus tous les 100 ms (*throttle*). Un téléchargement passe par un fichier `.part` puis un `rename`.

## 6. Tests

- Tout parseur ou résolveur reçoit des tests unitaires exhaustifs, y compris les cas d'erreur et les cas limites.
- Le protocole se teste contre un serveur `russh` en process (skill `ssh-testing`), sans réseau externe.
- L'async utilise `#[tokio::test]`, et `tokio::time::pause()` pour les timeouts et keepalives.
- Les fichiers passent par `tempfile::tempdir()` et `Paths::rooted(dir)`. On ne touche jamais le vrai `~/.ssh`.

## 7. Style

- Pour la visibilité : `pub(crate)` par défaut, `pub` seulement pour l'API consommée par les apps.
- Les noms sont explicites (`known_hosts_line` plutôt que `line`). Pas d'abréviations obscures.
- Chaque module commence par une doc `//!` qui donne son rôle, ses invariants et sa phase s'il est incomplet.
- Les constantes sont nommées pour toute valeur magique (timeouts, capacités, tailles de chunk).
