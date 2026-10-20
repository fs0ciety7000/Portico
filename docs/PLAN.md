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

### 1a. Transport et authentification ✅ *(Opus ; revues `agent-ssh-protocols` et `agent-security`)*
- [x] `session/` : un acteur par session, machine d'états `SessionPhase` émise à chaque transition, registre de sessions dans l'`Engine`
  (`connect` non bloquant, `disconnect`, `session_logs`)
- [x] `known_hosts` maison : hashed `|1|`, `[host]:port`, wildcards/négation, `@cert-authority` (validité, type, principals), `@revoked`,
  ajout atomique en `0600`, `HostKeyAlias`, préférence des algorithmes déjà connus
- [x] Clé inconnue : `Prompt::UnknownHostKey` (TOFU, avec avertissement s'il existe d'autres types de clé connus).
  Clé modifiée : `SessionEvent::HostKeyChanged` puis **échec dur**, même avec `StrictHostKeyChecking no`
- [x] Auth dans l'ordre d'OpenSSH : `none`, puis certificats, agent (`IdentityAgent`, `IdentitiesOnly`), clés `IdentityFile`
  (**passphrase demandée seulement si le serveur accepte la clé**, via un signer paresseux), keyboard-interactive, mot de passe.
  Les secrets peuvent être mémorisés via `SecretStore`
- [x] ProxyJump récursif (sémantique `ssh -J`), détection de cycles, au plus 8 hops, phases `Connecting { hop, hops }`
- [x] Timeouts : TCP, négociation (suspendue pendant un prompt), canal de jump. Keepalive avec **mesure du RTT**
  (`keepalive@openssh.com`), `ServerAliveInterval` / `ServerAliveCountMax`, détection de fermeture distante
- [x] Logs de session (`LogRing`) : route, adresses, vérification de clé, méthodes tentées (jamais de secret)
- [x] Tests : 27 de bout en bout (serveur russh en process) et **interop OpenSSH 9.6 réel** (8 scénarios), 11 tests `known_hosts`, 4 tests de route
- [x] TUI et GUI branchées : statut live (point coloré, phase, latence), toasts d'erreur avec hint, `d` pour déconnecter.
  Les prompts sont encore refusés proprement (les dialogues arrivent en 1b et 1c)

### 1b. PTY et terminal TUI *(Opus / Sonnet fort)*
- Reporté de 1a : interop Dropbear en CI ; ticket upstream russh (abandon d'un `Signer`, vérification au re-key)
- `Engine::open_terminal` : `request_pty` (TERM `xterm-256color`, modes), shell, resize, et `TerminalHandle` (plan de données borné)
- TUI : widget `TerminalView` sur `alacritty_terminal` (palette ANSI du thème, curseur, scrollback, sélection),
  onglets, `Ctrl-Space` comme préfixe pour les commandes Portico (le reste passe au remote)
- Prompts TUI : modales pour clé hôte (TOFU), clé modifiée (`HostKeyChanged`, bloquante), mot de passe, passphrase et keyboard-interactive
  (champ masqué, case « mémoriser »). Elles remplacent le refus automatique actuel
- Barre de statut : badge de phase animé et latence

### 1c. PTY et terminal GUI *(Opus pour le composant Terminal, puis Sonnet)*
- `xterm.js` + addon WebGL (repli DOM) + `fit`, octets via `tauri::ipc::Channel<Vec<u8>>`
- **Spike perf** WebKitGTK et WKWebView (débit et latence) : décision sur le renderer
- Onglets, modales de prompt (la commande `respond_prompt` existe déjà), reconnexion

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
| 0 Fondations | ✅ | 35 | couverte par la revue 1a | ✅ (captures TUI et GUI) |
| 1a Transport et auth | ✅ | 82 + interop OpenSSH (8 scénarios) | ✅ (voir ci-dessous) | statut live vérifié (tmux) |
| 1b / 1c Terminal TUI / GUI | 🔜 | | | |
| 2 SFTP / tunnels | ⏳ | | | |
| 3 Clés / secrets / snippets | ⏳ | | | |
| 4 Finition | ⏳ | | | |

## Revues de la phase 1a (2026-10-20)

### `agent-security` : 1 ÉLEVÉ, 2 MOYEN, 3 FAIBLE, tous traités
| Gravité | Constat | Suite donnée |
|---|---|---|
| ÉLEVÉ | Nom d'hôte non mis en minuscules : une clé **changée** sur une entrée hachée passait pour inconnue, donc était acceptée en `accept-new` | Corrigé : nom normalisé (`HostKeyAlias` ou `HostName`), test hashed + casse mixte |
| MOYEN | `accept-new`/`no` acceptait un nouvel algorithme pour un hôte déjà connu | Corrigé : refus `host_key_new_algorithm` + test |
| MOYEN | Mot de passe mémorisé indexé sur `HostName:Port` (fuite possible entre deux `HostKeyAlias`) | Corrigé : lié à l'identité vérifiée (nom known_hosts + port + empreinte de la clé hôte) |
| FAIBLE | Injection de motif dans known_hosts via `HostKeyAlias` | Corrigé : `validate_host_name` + test |
| FAIBLE | Re-key non revérifié | Limite de russh 0.64 (handler appelé seulement au premier KEX) ; documenté, ticket upstream à ouvrir |
| FAIBLE | Copies de mots de passe non effacées (russh, webview) | DTO Tauri désérialisé directement en `SecretString`. Désactivation des core dumps et du ptrace planifiée en phase 4 |
| (hors sécu) | La GUI cessait de relayer les événements après un `Lagged` | Corrigé |

### `agent-ssh-protocols` : 1 BLOQUANT, 9 IMPORTANT, 7 MINEUR
| Constat | Suite donnée |
|---|---|
| **BLOQUANT** : `ProxyCommand` ignoré, connexion directe au `HostName` | Corrigé : premier gagnant `ProxyCommand`/`ProxyJump` respecté, `ProxyCommand` refusé explicitement (`unsupported`) + 2 tests. Implémentation complète : post-MVP |
| SHA-1 `ssh-rsa` utilisé par défaut | Corrigé : retiré des algos hôte, clés RSA sautées sans `rsa-sha2-*` |
| Signature vide fragile (Dropbear), refus d'agent fatal | Corrigé : signature bien formée mais invalide, refus d'agent passant à l'identité suivante + test. Interop Dropbear en CI : à ajouter |
| Keepalive bloquant Disconnect/annulation | Corrigé : sonde concurrente au `select!` |
| keyboard-interactive désynchronisé après un prompt écarté | Corrigé : vidage jusqu'à la conclusion + contrôle du nombre de réponses + test |
| Pas de timeout pendant l'auth | Corrigé : délai de 60 s suspendu pendant les prompts |
| Certificat de l'agent signé par fichier | Corrigé : `AgentCert` prioritaire, dédupliqué |
| `IdentityFile k.pub` ignoré | Corrigé : identités publiques seules (agent) + test |
| `IdentityAgent ~/…` non expansé | Corrigé : `~`, `${VAR}`, tokens (et `GlobalKnownHostsFile`) |
| `PreferredAuthentications`, `*Authentication no`, `NumberOfPasswordPrompts` | Corrigé + test |
| RST non détecté, tâches russh orphelines | Corrigé : `WatchedStream` (EOF/erreur et abort) |
| MINEUR : délai épuisé juste après un prompt | Corrigé (nouvelle fenêtre après chaque prompt) |
| MINEUR : DISCONNECT de la cible coupé par celui du jump | Corrigé (fermeture hop par hop avec délai de grâce) |
| MINEUR : « Too many authentication failures » trompeur | Corrigé : code `auth_disconnected` + hint |
| MINEUR : `HashKnownHosts yes` et `ServerAliveInterval 0` ignorés | Corrigé |
| MINEUR : certificat hôte invalide sans repli sur la clé brute | Conservé volontairement (plus strict qu'OpenSSH), consigné dans STACK |
| MINEUR : re-key avec clé différente | Voir la ligne re-key de la revue sécurité (limite russh) |
