# Plan d'action : MVP Portico

> Piloté par l'orchestrateur (voir `CLAUDE.md` §1). Chaque phase a un **critère de sortie vérifiable**.
> Une phase n'est close qu'après la revue de `agent-security`, et de `agent-design-review` si elle a touché l'UI.
> Légende : ✅ fait · 🔜 prochaine · ⏳ à venir. Le modèle indiqué suit `CLAUDE.md` §10.

## Phase 0 : fondations ✅ *(Opus)*

- [x] Choix de stack documenté (`docs/STACK.md`)
- [x] `CLAUDE.md`, skills (`rust-strict`, `ui-design-premium`, `tui-patterns`, `gui-tauri-patterns`, `ssh-testing`)
- [x] Agents (`agent-security`, `agent-ssh-protocols`, `agent-design-review`)
- [x] Workspace Cargo, lints stricts, profils, `rust-toolchain.toml`, CI GitHub Actions, `deny.toml`
- [x] `portico-core` : erreurs typées, IDs, événements, `Engine` (plan de contrôle, prompts), store TOML atomique
- [x] **Parser `~/.ssh/config` complet** (Host, Match, Include, tokens, ProxyJump), 21 tests
- [x] Inventaire d'hôtes, métadonnées `hosts.toml` et recherche floue (`#tag`, `*` favoris)
- [x] Découverte des clés et certificats, `KeyringStore`, modèles snippets, forward et SFTP
- [x] `portico-theme` : 3 thèmes, tests de contraste WCAG, export CSS vérifié
- [x] TUI : shell (liste d'hôtes, recherche, détails, aide, toasts, thèmes)
- [x] GUI : shell Tauri + Svelte (sidebar, détails, palette `⌘K`, toasts, thèmes, mode démo navigateur)

**Sortie** : `cargo test` vert (35 tests), clippy propre, les deux UIs affichent les hôtes réels.

## Phase 1 : connexion interactive (cœur du produit) 🔜

### 1a. Transport et authentification *(Opus, puis revue `agent-ssh-protocols` et `agent-security`)*
- `session/` : un acteur par session et la machine d'états `SessionPhase` émise à chaque transition
- `transport.rs` : `russh::client` avec un `Handler` qui vérifie les clés via `known_hosts`
  (`UserKnownHostsFile` + `GlobalKnownHostsFile`, entrées hachées, `@cert-authority`, `@revoked`).
  Une clé inconnue déclenche `Prompt::UnknownHostKey` (TOFU) ; une clé modifiée provoque un **échec dur** avec `Prompt::ChangedHostKey`, en information seulement.
- Ordre d'authentification façon OpenSSH : **certificat** (`CertificateFile` ou `*-cert.pub`), puis **agent** (`SSH_AUTH_SOCK` / `IdentityAgent`),
  puis clés `IdentityFile` (passphrase via prompt, puis keyring), puis keyboard-interactive, puis mot de passe.
  `IdentitiesOnly` est respecté.
- **ProxyJump** : chaînage via `channel_open_direct_tcpip` sur le hop précédent, puis le transport suivant sur ce stream,
  avec la phase `Connecting { hop, hops }`. Les alias de jump sont résolus récursivement, avec détection de cycle.
- `ConnectTimeout`, keepalive (`ServerAliveInterval`, 15 s par défaut) avec **mesure de RTT** et `SessionEvent::Latency`
- Logs de session (`LogRing`) : kex négocié, algorithmes, méthodes d'auth tentées, hops
- **Tests** : serveur `russh` en process (skill `ssh-testing`) couvrant TOFU, clé changée, auth clé, auth cert, auth mot de passe, jump à 2 hops, timeout

### 1b. PTY et terminal TUI *(Opus / Sonnet fort)*
- `Engine::open_terminal` : `request_pty` (TERM `xterm-256color`, modes), shell, resize, et `TerminalHandle` (plan de données borné)
- TUI : widget `TerminalView` sur `alacritty_terminal` (palette ANSI du thème, curseur, scrollback, sélection),
  onglets, `Ctrl-Space` comme préfixe pour les commandes Portico (le reste passe au remote)
- Prompts TUI : modales pour clé hôte, mot de passe et passphrase (champ masqué, case « mémoriser »)
- Barre de statut : badge de phase animé et latence

### 1c. PTY et terminal GUI *(Opus pour le composant Terminal, puis Sonnet)*
- `xterm.js` + addon WebGL (repli DOM) + `fit`, octets via `tauri::ipc::Channel<Vec<u8>>`
- **Spike perf** WebKitGTK et WKWebView (débit et latence) : décision sur le renderer
- Onglets, modales de prompt, badge de statut avec latence, reconnexion

**Sortie** : connexion réelle à `localhost` et via un bastion, depuis les deux UIs. `vim` et `htop` sont utilisables. Statut et latence sont visibles.

## Phase 2 : SFTP et forwarding ⏳ *(Opus pour le core, Sonnet pour l'UI)*
- `sftp/` : ouverture du sous-système sur la session, `list`, `stat`, `mkdir`, `rename`, `remove`, et transferts streamés
  (chunks de 256 Kio, progression, annulation, `OnConflict`)
- Explorateur TUI (deux panneaux local/distant, style Midnight Commander) et GUI (liste, fil d'Ariane, glisser-déposer)
- `forward/` : `-L` (listener tokio, puis `direct-tcpip` par connexion) et `-R` (`tcpip-forward` + `server_channel_open_forwarded_tcpip`).
  Bind par défaut sur `localhost`. Les forwards de `ssh_config` sont proposés et non démarrés automatiquement (opt-in par hôte).
- Panneau « Tunnels » : état, connexions actives, arrêt

**Sortie** : upload et download d'un fichier de 1 Gio avec progression et annulation, et un tunnel `-L` vers une base distante fonctionnel.

## Phase 3 : clés, secrets et snippets ⏳ *(Opus ; revue `agent-security` obligatoire)*
- Génération : ed25519 (défaut), RSA ≥ 3072, ECDSA P-256/384/521, passphrase (bcrypt-pbkdf, OpenSSH),
  écriture en `0600` et `.pub` en `0644`, **refus d'écraser** une clé existante
- Copy-id : auth initiale (mot de passe ou keyboard-interactive), puis `mkdir -p ~/.ssh && chmod 700`, ajout **idempotent**
  à `authorized_keys` (pas de doublon), `chmod 600`, et vérification par reconnexion avec la clé
- Agent : liste des identités, ajout et suppression (`ssh-add`) ; certificats affichés avec leur validité et une alerte d'expiration
- Vault chiffré de repli (Argon2id + XChaCha20-Poly1305) avec déverrouillage au démarrage
- Snippets : CRUD dans les deux UIs, portée (global, hôtes, tags), placeholders, exécution dans l'onglet actif (palette `⌘K`)

**Sortie** : générer une clé, la copier sur un serveur et se reconnecter sans mot de passe, le tout sans quitter Portico.

## Phase 4 : finition MVP ⏳ *(Sonnet, revues Opus)*
- Sessions récentes (`state/recent.toml`), favoris, édition des tags et groupes
- Panneau debug : logs par session, filtrables et copiables, avec la vue « config effective » (`ssh -G`)
- Raccourcis configurables (`settings.toml`) et doc `KEYBINDINGS.md` générée depuis la keymap
- Packaging : `.dmg` (signature et notarisation), `.deb`, AppImage, binaire TUI (cargo-dist ou équivalent)
- Mesures : démarrage à froid, RAM au repos et avec 5 sessions, comparées aux objectifs de `CLAUDE.md` §2

**Sortie MVP** : les 14 fonctionnalités de `CLAUDE.md` §7 sont démontrables dans les deux UIs. Audit sécurité final.

---

## Roadmap post-MVP

| Thème | Contenu |
|---|---|
| Multi-hop avancé | `ProxyCommand` arbitraire (process stdio comme transport), repli « system ssh » pour `Match exec`, visualisation du chemin de hops avec latence par hop |
| Forwarding | `-D` (SOCKS5), forwards persistants avec redémarrage auto, forwarding d'agent (opt-in et averti) |
| Vault enrichi | Plusieurs coffres, export et import chiffrés, déverrouillage biométrique (Touch ID via Keychain `protected`) |
| Enregistrement de session | Format asciicast v2, rejouable, avec masquage des saisies de mot de passe |
| Synchronisation locale optionnelle | Sync des métadonnées et snippets via un dossier (Syncthing, git) ou un fichier chiffré : **jamais de cloud imposé** |
| Terminal | Split panes, broadcast d'input multi-hôtes (avec garde-fou), recherche dans le scrollback, liens cliquables |
| Plateformes | Windows (Credential Manager, ConPTY non requis côté remote), thème clair |
| GUI native | Réévaluation de `gpui` si la GUI web atteint ses limites de performance |
| Intégrations | Mosh, Teleport ou certificats éphémères (CA SSH d'entreprise), import depuis Termius |

## Tableau de bord

| Phase | État | Tests | Revue sécu | Revue design |
|---|---|---|---|---|
| 0 Fondations | ✅ | 35 | à planifier en ouverture de 1a | ✅ (captures TUI et GUI) |
| 1 Connexion | 🔜 | | | |
| 2 SFTP / tunnels | ⏳ | | | |
| 3 Clés / secrets / snippets | ⏳ | | | |
| 4 Finition | ⏳ | | | |
