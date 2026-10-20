# CLAUDE.md — Portico

> **Portico** : gestionnaire de sessions SSH moderne, *local-first*, rapide et *keyboard-first*,
> pour macOS et Linux. Alternative ouverte à Termius, sans cloud ni lock-in.
> Un moteur Rust (`portico-core`) et deux interfaces : une **TUI** et une **GUI**.

Ce fichier est le contrat du projet. Toute contribution (humaine ou agent) le respecte.
En cas de conflit entre ce fichier et une demande ponctuelle, signaler le conflit avant d'agir.

---

## 1. Rôle de Claude : chef de projet et orchestrateur

Claude n'est pas qu'un exécutant. Sur Portico, il :

1. **Priorise** : suit `docs/PLAN.md` (phases, critères de sortie), refuse le *scope creep* hors phase.
2. **Décide et trace** : tout choix structurant (crate, pattern, format de données) est consigné
   dans `docs/STACK.md` (section *Decision log*) avec alternatives et raison.
3. **Délègue** aux agents spécialisés (`.claude/agents/`) aux moments définis en §9.
4. **Charge les skills** pertinents avant d'écrire du code (`.claude/skills/`, voir §8).
5. **Garde la barre** : rien n'est « fini » sans tests, `clippy` propre, et un rendu UI vérifié.
6. **Rend compte** : à la fin d'une tâche, il dit ce qui est fait, vérifié, et ce qui reste.

## 2. Vision produit et principes non négociables

| Principe | Conséquence concrète |
|---|---|
| **Zéro lock-in** | Tout fonctionne hors ligne. Aucun compte et aucun service distant requis. Les données Portico sont en TOML lisible. |
| **Respect du setup existant** | `~/.ssh/config`, `known_hosts`, `ssh-agent` et les clés locales sont la source de vérité. Portico **lit** `~/.ssh/config` et ne le **réécrit jamais**. Ses métadonnées (tags, favoris, groupes) vivent dans `hosts.toml`, indexées par alias. Les seules écritures dans `~/.ssh` sont l'ajout dans `known_hosts` après accord explicite et la génération de clés demandée par l'utilisateur. |
| **Keyboard-first** | Toute action principale a un raccourci. La palette de commandes (`⌘K` / `Ctrl-K`) expose tout. Les raccourcis sont documentés dans `docs/KEYBINDINGS.md`. |
| **Performance** | La TUI doit être utilisable en moins de 100 ms à froid, la GUI afficher sa première fenêtre en moins de 500 ms. Aucun redraw quand rien ne change. Le chargement des hôtes ne bloque jamais l'UI. |
| **Sécurité by design** | Aucun secret en clair, ni sur disque, ni dans les logs, ni dans les événements. Les secrets sont stockés dans le keyring système, ou à défaut dans un vault chiffré. Un changement de clé hôte provoque toujours un refus bloquant. |
| **Événementiel** | Le moteur publie des `EngineEvent` (plan de contrôle), et les flux d'octets circulent sur des canaux dédiés (plan de données). Voir §4. |

## 3. Identité visuelle et exigences design

**Direction** : minimalisme architectural. Le logo est un portique : un entablement sur deux colonnes
qui encadrent une arche (`design/portico-mark.svg`), accompagné d'un wordmark en capitales espacées (Geist)
(`design/portico-wordmark.svg`). L'ensemble doit faire premium, rester sobre, être monochrome
et adapté au mode sombre.

- **Le design est une priorité absolue**, au même rang que la correction fonctionnelle.
- Surfaces minérales sombres, texte « marbre », **un seul accent** métallique par thème.
- **La couleur saturée est réservée au sens** : statuts de connexion, erreurs, avertissements.
- Statuts toujours visibles et soignés : `idle → connecting → authenticating → connected` avec latence, ou `error` avec un *hint* actionnable.
- Micro-interactions de 120 à 260 ms maximum, courbe `--ease-standard`, en respectant `prefers-reduced-motion`.
- La hiérarchie passe par la taille, la graisse et l'espace, jamais par des bordures épaisses.
- Il faut une sensation de produit fini dès le MVP : états vides, chargements (skeleton ou spinner), erreurs, focus visible.
- **Source unique des tokens** : `crates/portico-theme`. La GUI consomme le CSS généré
  (`cargo run -p portico-theme --bin export-tokens`) et un test échoue si ce CSS est périmé.
  **Aucune couleur en dur** dans les composants.
- Thèmes : **Obsidian** (défaut, accent laiton), **Basalt** (gris-bleu, acier), **Travertine Night** (pierre chaude, terracotta).
  Le contraste est testé : texte principal AAA, texte secondaire AA.

Les règles détaillées se trouvent dans le skill `ui-design-premium`.

## 4. Architecture

```
Portico/
├── Cargo.toml                  # workspace (resolver 3, edition 2024, lints stricts)
├── crates/
│   ├── portico-core/           # moteur métier — AUCUNE dépendance UI
│   │   └── src/
│   │       ├── engine.rs       # Engine (handle clonable), commandes, prompts, plan données
│   │       ├── events.rs       # EngineEvent, SessionPhase, Prompt… (plan de contrôle)
│   │       ├── error.rs        # Error (thiserror) + ErrorSummary (code, message, hint)
│   │       ├── ids.rs          # SessionId, TerminalId, TunnelId, SftpId, PromptId…
│   │       ├── ssh_config/     # parser OpenSSH : Host/Match/Include, tokens, ProxyJump
│   │       ├── hosts.rs        # inventaire = ssh_config + métadonnées hosts.toml
│   │       ├── search.rs       # recherche floue (nucleo), #tag, * favoris
│   │       ├── keys.rs         # découverte, certificats, génération, copy-id
│   │       ├── secrets.rs      # SecretStore : keyring système (+ vault chiffré)
│   │       ├── forward.rs      # specs -L / -R (+ -D post-MVP)
│   │       ├── sftp.rs         # modèle explorateur + transferts
│   │       ├── snippets.rs     # snippets globaux / par hôte / par tag, placeholders
│   │       ├── logs.rs         # LogRing par session (panneau debug)
│   │       └── store.rs        # chemins XDG/macOS, TOML atomique 0600
│   └── portico-theme/          # design tokens (Rust) → ratatui + CSS généré
├── apps/
│   ├── portico-tui/            # binaire `portico` (ratatui + crossterm + alacritty_terminal)
│   └── portico-gui/            # Tauri 2 + Svelte 5 + xterm.js
│       ├── src/                # frontend (TypeScript, Svelte runes)
│       └── src-tauri/          # shell Rust mince : commandes IPC → Engine
├── design/                     # logo, wordmark
├── docs/                       # STACK (décisions), PLAN (MVP + roadmap), KEYBINDINGS
└── .claude/{skills,agents}/
```

**Règles de dépendance** : `portico-core` ne dépend ni de `ratatui`, ni de `tauri`, ni d'aucun crate UI.
`portico-theme` ne dépend que de `serde`. Les apps dépendent de `portico-core` et `portico-theme`, jamais l'inverse.

### Modèle d'exécution du moteur

- **Commandes** : méthodes `async` sur `Engine` (handle `Arc`, `Clone`).
- **Plan de contrôle** : `tokio::sync::broadcast<EngineEvent>`. Les UIs dérivent leur état des événements.
  Un abonné en retard reçoit `Lagged` et doit resynchroniser via des requêtes.
- **Plan de données** : chaque terminal reçoit un `TerminalHandle` composé de deux `mpsc` **bornés**
  (entrée : clavier et resize ; sortie : octets PTY). La backpressure remonte jusqu'à la fenêtre SSH,
  jamais jusqu'au bus d'événements. Dans la GUI, les octets passent par `tauri::ipc::Channel`.
- **Prompts** : la tâche qui a besoin de l'utilisateur (clé hôte, mot de passe, passphrase,
  keyboard-interactive) enregistre un `oneshot` sous un `PromptId`, émet `EngineEvent::Prompt`,
  puis attend `Engine::respond`. Seule cette opération est suspendue.
- **Acteurs** : une tâche par session possède le `russh::client::Handle`. Les canaux PTY, SFTP et
  forwards sont des tâches enfants sous un `CancellationToken`, si bien que fermer la session ferme tout.
- **Bloquant** : la lecture de fichiers et le keyring passent par `spawn_blocking`. Rien de bloquant dans une tâche async.

## 5. Stack retenue (résumé, détails et alternatives dans `docs/STACK.md`)

| Domaine | Choix | Pourquoi |
|---|---|---|
| Async | `tokio` 1.x | Écosystème de russh, de Tauri et de crossterm `EventStream` |
| SSH | `russh` 0.64 (pur Rust, backend `aws-lc-rs`) | Async natif, PTY, forwarding `-L/-R`, agent, **certificats OpenSSH**, `known_hosts`, sans libssh2 en C |
| SFTP | `russh-sftp` 3 | Sous-système SFTP sur le même transport russh |
| Clés / certs | `russh::keys::ssh_key` (version épinglée par russh) | Un seul modèle de clé dans le process : génération ed25519/rsa/ecdsa, chiffrement OpenSSH, certificats |
| Config SSH | parser maison (`ssh_config/`) | Sémantique OpenSSH exacte (first-wins, Include, Match, tokens), testée. Les crates existants sont incomplets |
| Secrets | `keyring` 4 (Keychain macOS, Secret Service Linux) + vault `argon2` + `chacha20poly1305` ; `secrecy` et `zeroize` | Local, natif, sans clair |
| Recherche | `nucleo-matcher` | Matcher de Helix, très rapide, smart-case |
| Données | TOML (`toml`, `serde`) + `directories` | Lisible, versionnable, sans lock-in |
| Erreurs / logs | `thiserror` (core), `color-eyre` (apps), `tracing` | Erreurs typées, diagnostics riches |
| TUI | `ratatui` 0.30 + `crossterm` 0.29 + `alacritty_terminal` 0.26 | Standard de facto, rendu immédiat, émulateur VT robuste |
| GUI | **Tauri 2** + **Svelte 5** + TypeScript + **xterm.js 6** (WebGL) | Meilleur rapport design premium / vélocité / empreinte. Cœur Rust partagé, ~10× plus léger qu'Electron |
| Thème | `portico-theme` (tokens Rust → ratatui + CSS) | Une seule source de vérité visuelle |

## 6. Conventions de code (strictes)

### Rust
- **Edition 2024**, MSRV `1.90`, `cargo fmt`, et `cargo clippy --all-targets` **sans warning**
  (`pedantic` activé au niveau workspace).
- **Zéro `unwrap()`, `expect()`, `panic!`, `todo!`, `unimplemented!`, `dbg!` hors tests** : c'est un lint `deny` dans le workspace.
  On utilise `?`, `ok_or`, `unwrap_or_else`, ou `lock().unwrap_or_else(PoisonError::into_inner)`.
- `#![forbid(unsafe_code)]` est actif au niveau workspace. Une exception exige une ADR dans `docs/STACK.md`.
- **Erreurs typées** : un enum `thiserror` par module, agrégé dans `portico_core::Error`, avec `#[non_exhaustive]`.
  Pas d'`anyhow` dans `portico-core`. Les UIs affichent `Error::summary()` (code, message, hint)
  et ne font **jamais** de *string matching*.
- Un message d'erreur est une phrase courte, sans majuscule initiale, avec le contexte utile (chemin, ligne, hôte) et **sans secret**.
- Les types publics dérivent `Debug`, et un secret a un `Debug` caviardé (`SecretString`).
- Indexation via `.get()` (lint `indexing_slicing`), arithmétique explicite (`saturating_*` et `checked_*` aux frontières).
- Visibilité minimale (`pub(crate)` par défaut dans les binaires, lint `unreachable_pub`).
- Doc `//!` en tête de chaque module : rôle, invariants, phase de livraison si c'est un squelette.
- Les `#[allow(...)]` sont ciblés et **commentés** (pourquoi). Jamais de `allow` global sans raison.
- **Tests** : unitaires à côté du code (`#[cfg(test)]`), avec fichiers temporaires via `tempfile`.
  Toute logique de parsing, de résolution ou de sécurité est testée, y compris les cas d'erreur.
  Les tests de protocole utilisent un serveur `russh` en process (aucun réseau externe en CI).
- Les commits suivent le style *conventional commits* (`feat(core): …`, `fix(tui): …`). Une PR doit être verte.

### TypeScript / Svelte (GUI)
- Svelte 5 **runes** (`$state`, `$derived`, `$effect`, `$props`), TypeScript `strict` et `noUncheckedIndexedAccess`.
- `pnpm check` (svelte-check) sans erreur. Aucune couleur, taille ou durée en dur : uniquement les `var(--…)` de `tokens.css`.
- Les types IPC vivent dans `src/lib/api.ts` et reflètent les DTO Rust (`src-tauri/src/dto.rs`).
- La CSP est stricte (`tauri.conf.json`) et les capabilities minimales (`capabilities/default.json`).

### Commandes de vérification (avant tout « c'est fait »)
```bash
cargo fmt --all --check
cargo clippy --all-targets            # core + theme + tui (membres par défaut)
cargo test
cargo clippy -p portico-gui           # nécessite webkit2gtk-4.1 sur Linux
(cd apps/portico-gui && pnpm install && pnpm check && pnpm build)
```

## 7. Fonctionnalités MVP

1. Parsing de `~/.ssh/config` (ProxyJump, wildcards, héritage, Include, Match) ✅ *(livré en phase 0)*
2. Recherche floue, tags et groupes légers ✅ *(moteur livré, édition des tags en phase 1)*
3. Connexion SSH interactive (PTY) dans les deux UIs
4. SFTP : explorateur, navigation, upload et download
5. Clés locales, ssh-agent et **certificats SSH** *(découverte livrée)*
6. Génération de clés (ed25519 / rsa ≥ 3072 / ecdsa) avec passphrase
7. Copie de clé publique (équivalent de `ssh-copy-id`)
8. Port forwarding local et distant
9. Sessions récentes et favorites, onglets multi-sessions
10. Snippets (création, édition, exécution par hôte ou global, placeholders) *(modèle livré)*
11. Statut de connexion élégant et latence
12. 3 thèmes sombres premium et raccourcis documentés ✅
13. Secrets dans le keyring (vault chiffré en repli)
14. Mode debug : logs de connexion par session dans l'UI

Le plan détaillé et la roadmap post-MVP sont dans `docs/PLAN.md`.

## 8. Skills (`.claude/skills/`) : quand les charger

| Skill | Charger avant de… |
|---|---|
| `rust-strict` | écrire ou relire du Rust, en particulier `portico-core` |
| `ui-design-premium` | toucher à un écran, un composant, un thème ou un token, TUI comme GUI |
| `tui-patterns` | travailler dans `apps/portico-tui` |
| `gui-tauri-patterns` | travailler dans `apps/portico-gui` (IPC, xterm.js, Svelte) |
| `ssh-testing` | tester sessions, auth, forwarding ou SFTP avec un serveur russh en process |

## 9. Agents (`.claude/agents/`) : quand les invoquer

| Agent | Invoquer quand |
|---|---|
| `agent-security` | Toute modification de `secrets.rs`, `keys.rs`, de la vérification `known_hosts`, du vault, de la génération ou copie de clés, de la CSP ou des capabilities Tauri. **Obligatoire avant la clôture de chaque phase.** |
| `agent-ssh-protocols` | Conception ou revue du transport, de l'authentification, de ProxyJump, du PTY, du forwarding, de SFTP, des certificats ou de copy-id. |
| `agent-design-review` | Après chaque écran ou composant notable, et avant la clôture d'une phase (captures TUI et GUI). |

L'orchestrateur résume au user les conclusions de chaque agent : les rapports d'agent ne lui sont pas visibles.

## 10. Recommandations d'effort modèle

| Travail | Modèle |
|---|---|
| Architecture, CLAUDE.md, skills/agents, structure, choix de stack | le plus fort (Opus) |
| `portico-core` : SSH, SFTP, clés, tunnels, certificats, secrets | fort (Opus) |
| TUI et parties interactives complexes (terminal, SFTP browser, formulaires) | Opus ou Sonnet fort |
| GUI : setup, design system, composants critiques (terminal, prompts, statut) | Opus ; volume de composants simples en Sonnet |
| Itérations, correctifs, features secondaires, docs | Sonnet (ou équivalent plus léger) |
| Revue sécurité et protocoles | fort (Opus), via les agents dédiés |

## 11. Interdits

- Écrire dans `~/.ssh/config`. Désactiver la vérification des clés hôtes. Accepter en un clic une clé hôte **modifiée**.
- Logger un secret, une passphrase, un mot de passe, une clé privée ou un contenu de terminal en clair.
- Bloquer le runtime async (I/O sync, keyring, `std::thread::sleep`) ou le thread UI.
- Ajouter un service réseau tiers (télémétrie, sync cloud) sans opt-in explicite et une ADR.
- Introduire une dépendance UI dans `portico-core`.
