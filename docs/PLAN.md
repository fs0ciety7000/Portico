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

## Phase 1 : connexion interactive (cœur du produit) ✅

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

### 1b. PTY et terminal TUI ✅ *(Opus)*
- [x] `Engine::open_terminal` : canal session, `pty-req` (`xterm-256color`, `VERASE`, `IUTF8`), shell, `window-change`,
  exit-status/signal, fermeture ; plan de données borné avec backpressure bout en bout. Une demande faite pendant la connexion
  est servie dès que la session est établie
- [x] TUI : émulateur `alacritty_terminal` par onglet (10 000 lignes d'historique, OSC 52 désactivé, réponses DA/DSR
  renvoyées, titre OSC dans l'onglet), rendu ANSI 16 couleurs du thème / 256 couleurs / truecolor, curseur couleur accent
- [x] Onglets (Hosts + terminaux) avec préfixe `Ctrl-Space`, encodage des touches xterm, bracketed paste,
  historique `Shift-PgUp`, redimensionnement du PTY en direct
- [x] Dialogues : TOFU (`yes` tapé quand la clé est inhabituelle), mot de passe et passphrase (masque fixe, case « mémoriser »),
  keyboard-interactive, alerte bloquante « clé changée »
- [x] Vérifié en réel contre OpenSSH : vim (écran alternatif), couleurs, `Ctrl-C`, historique, ProxyJump, resize, passphrase, alerte.
  500 000 lignes affichées en environ 0,5 s ; mémoire 7 Mo au repos, 39 Mo avec un historique plein
- Reporté : interop Dropbear en CI ; ticket upstream russh (abandon d'un `Signer`, vérification au re-key) ;
  sélection et copie à la souris (le terminal hôte reste utilisable avec `Shift`)

### 1c. PTY et terminal GUI ✅ *(Opus)*
- `xterm.js` + addon WebGL (repli DOM) + `fit`, octets via `tauri::ipc::Channel<Vec<u8>>`
- **Spike perf** WebKitGTK et WKWebView (débit et latence) : décision sur le renderer
- Onglets, modales de prompt (la commande `respond_prompt` existe déjà), reconnexion

**Sortie** : connexion réelle à `localhost` et via un bastion, depuis les deux UIs. `vim` et `htop` sont utilisables. Statut et latence sont visibles.

## Phase 2 : SFTP et forwarding ✅ *(Opus)*
- [x] `sftp/` : un client par session, `list`, `stat`, `mkdir`, `rename`, `remove`, transferts streamés
  (progression, annulation, `OnConflict`), fichier partiel exclusif puis finalisation atomique, noms hostiles neutralisés
- [x] Explorateur TUI (deux panneaux local/distant, style Midnight Commander) et GUI (liste, fil d'Ariane, glisser-déposer,
  jetons d'upload émis par Rust, dossier de téléchargement tenu par Rust)
- [x] `forward/` : `-L` (toutes les adresses du bind) et `-R`, bind par défaut sur `localhost`, écoute exposée sur confirmation armée.
  Les forwards de `ssh_config` sont proposés, jamais démarrés d'office
- [x] Panneau « Tunnels » dans les deux UIs : état, connexions actives, arrêt, erreurs avec hint
- [x] Les deux UIs suivent la session (phase pendant la connexion, erreur + hint + reconnexion, jamais après une clé changée)
  et partagent la même grammaire clavier
- Reporté : transferts récursifs (dossiers), restauration du dossier distant après reconnexion, `posix-rename@openssh.com`

**Sortie** ✅ : 1 Gio envoyé en 4,1 s et rapatrié en 4,8 s contre OpenSSH réel (contenu identique, progression continue),
annulation à 25 % sans fichier résiduel des deux côtés (`PORTICO_INTEROP_BIG=1`) ; tunnels `-L`/`-R` vérifiés contre OpenSSH
(service écho, scénarios 10 et 11) et en TUI réelle (`nc` sur une écoute exposée). Le tunnel vers une vraie base de données
reste à démontrer manuellement.

## Phase 3 : clés, secrets et snippets ✅ *(Opus ; revues `agent-security`, `agent-ssh-protocols`, `agent-design-review`)*
- [x] Génération : ed25519 (défaut), RSA 3072–8192, ECDSA P-256/384/521, passphrase (format OpenSSH, bcrypt-pbkdf),
  privée créée en exclusif `0600`, `.pub` `0644`, **jamais d'écrasement**, aléa système, `~/.ssh` partagé refusé
- [x] Copy-id : sur la session authentifiée (mot de passe, keyboard-interactive…), script POSIX d'une ligne (sh, dash, bash, fish,
  tcsh, csh), clé sur stdin, ajout **idempotent** (blob, lignes commentées ignorées), aucune réécriture ni `chmod` de l'existant,
  vérification par reconnexion avec **cette clé seule** (ni certificat, ni `none`), même compte
- [x] Agent : liste, ajout (passphrase, durée de vie) et retrait vérifié ; certificats avec validité et alerte d'expiration (7 jours)
- [x] Vault chiffré de repli `vault.toml` v2 (Argon2id 64 Mio/t=3 + XChaCha20-Poly1305, AAD d'en-tête, verrou de fichier,
  compteur anti-retour), déverrouillage au démarrage, création proposée depuis « remember »
- [x] Snippets : CRUD dans les deux UIs, portée (global, hôtes, tags), placeholders validés, rendu sûr (aucun contrôle, refus du
  collage multi-ligne sans bracketed paste), exécution dans l'onglet actif (palette GUI « Run: … »)
- Reporté : ajout d'un certificat à l'agent et retrait d'un certificat (limite russh), commentaire des clés ajoutées à l'agent,
  expiration restante des identités de l'agent, copy-id par SFTP (comptes sans shell) et Windows, couleur du point de statut du vault

**Sortie** ✅ : en TUI contre OpenSSH réel, clé générée dans Portico (avec passphrase), installée par copy-id, vérifiée (prompt de
passphrase de la sonde), puis reconnexion avec cette clé seule acceptée par sshd, ajout à l'agent et reconnexion depuis Portico.
Couvert aussi par le scénario d'interop 12. En GUI : parcours complet vérifié en mode démo (pas encore dans l'app de bureau réelle).

## Phase 4 : finition MVP ✅ *(Opus ; revues `agent-security` ×2 et `agent-design-review`)*
- [x] Sessions récentes (`state/recent.toml`, 50 entrées), favoris (`*` / `⌘D`), édition des tags, groupes et notes (`e`) dans
  `hosts.toml` (verrou, clés et commentaires conservés, validation identique en lecture), sections Recent / groupes / Other, filtre `@groupe`
- [x] Panneau de logs de session (`l` / `⌘⇧L`) : sessions colorées par issue, logs filtrables (texte, niveau), suivi en direct,
  copie en texte borné ; onglet « Effective config » (`ssh -G` avec origines `file:line`, route ProxyJump, options sensibles masquées
  et jamais copiées) ; logs conservés pour les 20 dernières sessions terminées
- [x] Raccourcis configurables (`settings.toml`, `[keymap.tui]` / `[keymap.gui]`, entrées invalides signalées), thème mémorisé,
  tables de `KEYBINDINGS.md` **générées** depuis chaque keymap avec test de fraîcheur
- [x] Packaging : `.deb` et AppImage construits et lancés ici, tarballs TUI (glibc et musl statique) ; workflow de release (tag `v*`,
  6 cibles TUI, bundles GUI, `SHA256SUMS`, Release brouillon) ; signature et notarisation macOS isolées dans un environnement `release`
  (secrets du propriétaire requis, voir `docs/RELEASE.md`) ; outillage AppImage et actions épinglés
- [x] Mesures (`docs/PERF.md`, `bench/`) : objectifs TUI tenus, fenêtre GUI tenue ; chargement des gros `ssh_config` rendu linéaire
- [x] Durcissement du processus (pas de core dump, pas de `ptrace` sous Linux), CLI TUI (`--help`, `--version`, sans terminal)
- [x] Audit sécurité final, `SECURITY.md`, licences MIT et Apache 2.0, `cargo deny` vert

**Sortie MVP** ✅ : les 14 fonctionnalités de `CLAUDE.md` §7 dans les deux UIs (matrice ci-dessous). Audit sécurité final passé.

### Matrice de sortie du MVP

| # | Fonctionnalité | TUI | GUI | Preuve |
|---|---|---|---|---|
| 1 | Parsing de `~/.ssh/config` (ProxyJump, wildcards, héritage, Include, Match) | ✅ | ✅ | 23 tests du parser, interop OpenSSH, index linéaire |
| 2 | Recherche floue, tags et groupes | ✅ | ✅ | `#tag`, `@groupe`, `*` ; édition `e` ; tests core et UI |
| 3 | Connexion SSH interactive (PTY) | ✅ | ✅ | Vérifiée en réel (vim, top, resize, ProxyJump) en 1b/1c |
| 4 | SFTP : explorateur, upload, download | ✅ | ✅ | 1 Gio dans chaque sens contre OpenSSH, annulation sans résidu |
| 5 | Clés locales, ssh-agent, certificats | ✅ | ✅ | Interop scénario 12 (agent réel), badges de validité |
| 6 | Génération de clés (ed25519 / rsa ≥ 3072 / ecdsa) avec passphrase | ✅ | ✅ | Tests par algorithme, relecture par `ssh-keygen` |
| 7 | Copie de clé publique (`ssh-copy-id`) | ✅ | ✅ (démo) | Réel en TUI et en interop ; GUI en mode démo |
| 8 | Port forwarding local et distant | ✅ | ✅ | Interop scénarios 10 et 11, `nc` sur un tunnel exposé |
| 9 | Sessions récentes et favorites, onglets multi-sessions | ✅ | ✅ | Tests d'intégration (récents), rendu 80×24, captures |
| 10 | Snippets (CRUD, portée, placeholders, exécution) | ✅ | ✅ | Tests de rendu sûr, exécution réelle sous `sh` en TUI |
| 11 | Statut de connexion et latence | ✅ | ✅ | Barre de statut vérifiée de 880 à 1400 px, tmux 80×24 |
| 12 | 3 thèmes sombres et raccourcis documentés | ✅ | ✅ | Tests de contraste WCAG, `KEYBINDINGS.md` généré et testé |
| 13 | Secrets dans le keyring (vault chiffré en repli) | ✅ | ✅ | Vault v2 testé (verrou, anti-retour), déverrouillage au démarrage |
| 14 | Mode debug : logs de connexion par session | ✅ | ✅ | Panneau de logs, sessions terminées conservées |

**Non vérifié dans l'app de bureau réelle** : les parcours des phases 3 et 4 de la GUI (clés, copy-id, vault, snippets, logs, édition)
ont été vérifiés en mode démo (Playwright) et par les tests Rust des commandes, pas dans l'app Tauri sous WebKitGTK avec un vrai
serveur ; l'app réelle a été vérifiée jusqu'à la phase 1c (et la phase 2 pour le panneau de fichiers). À faire avant la première
release publique, avec la re-mesure du CPU au repos de la GUI (`bench/run-all.sh`).

---

## Après le MVP : phases 5 à 8 (ordre validé le 2026-10-21)

Mêmes règles que pour le MVP : une phase n'est close qu'après `agent-security` (et `agent-design-review` si elle
touche l'UI), et tout choix structurant passe par une ADR dans `docs/STACK.md`.

### Phase 5 : gains rapides et distribution ⏳ *(Opus pour le core, Sonnet pour l'UI)*
- `-D` (SOCKS5) dans `forward/` : SOCKS5 sans authentification (CONNECT, IPv4/IPv6/domaine), écoute sur `localhost`
  par défaut, écoute exposée sur confirmation armée, panneau Tunnels des deux UIs ; `DynamicForward` de `ssh_config` proposé
- Distribution : tap **Homebrew** (formula TUI, cask GUI) et paquet **AUR** `-bin`, générés depuis les artefacts et
  checksums de la release, mis à jour par le workflow de release
- Import **Termius** (après vérification du format d'export réel) : génère un fichier de config dans le dossier de Portico
  (jamais dans `~/.ssh`) et indique la ligne `Include` à ajouter soi-même ; tags et groupes importés dans `hosts.toml`.
  Aucun secret importé en clair

**Sortie** : `curl --socks5-hostname localhost:1080` passe par un hôte via Portico (les deux UIs) ; `brew install` et
l'installation depuis l'AUR fonctionnent sur une release taguée ; un export Termius de test donne les mêmes hôtes dans Portico.

### Phase 6 : terminal ⏳
- **Split panes** : divisions horizontales et verticales dans un onglet, focus et redimensionnement au clavier, chaque
  panneau étant un terminal indépendant (même session ou autre hôte) ; TUI et GUI
- **Enregistrement asciicast v2** (ADR préalable : exception encadrée à l'interdit « contenu de terminal en clair ») :
  opt-in explicite par session, indicateur « REC » toujours visible, sortie seulement (les saisies, donc les mots de passe,
  ne sont pas enregistrées), fichiers `0600`, avertissement, chiffrement optionnel ; relecture dans les deux UIs

**Sortie** : quatre panneaux sur deux hôtes utilisables au clavier seul ; une session enregistrée se relit à l'identique.

### Phase 7 : synchronisation locale optionnelle ⏳
- Sync de `hosts.toml`, des snippets et des réglages via un dossier choisi (Syncthing, git…) : **jamais de cloud imposé,
  jamais de secret** (le vault reste local, ou s'exporte chiffré à part) ; fusion par entrée, conflits montrés à
  l'utilisateur et jamais résolus en silence

**Sortie** : deux machines partagent métadonnées et snippets via un dossier synchronisé, conflit d'édition compris.

### Phase 8 : Windows ⏳ *(revue `agent-security` complète)*
- Gestionnaire d'identifiants Windows (`keyring`), agent OpenSSH sur named pipe, WebView2, chemins `%USERPROFILE%\.ssh`
- **Modèle de sécurité des fichiers refait avec des ACL** (équivalents de `0600`/`0700`, refus des liens), durcissement du
  processus côté Windows
- Distribution **Scoop** et **Winget**, installeur signé

**Sortie** : les 14 fonctionnalités du MVP sur Windows 11 dans les deux UIs ; audit sécurité Windows.

### À évaluer séparément (ADR et prototype avant toute décision)
| Sujet | Question à trancher |
|---|---|
| Mosh | Piloter `mosh-client` à travers un PTY local (pragmatique) ou réimplémenter SSP/UDP en Rust (gros chantier, crypto à revoir) ; revue `agent-ssh-protocols` |
| Snap / Flatpak | Le confinement strict bloque `~/.ssh` (interface `ssh-keys` à connecter à la main, mode classic soumis au Store) : publier ou non, et sous quelle forme |

## Roadmap au-delà

| Thème | Contenu |
|---|---|
| Multi-hop avancé | `ProxyCommand` arbitraire (process stdio comme transport), repli « system ssh » pour `Match exec`, visualisation du chemin de hops avec latence par hop |
| Forwarding | Forwards persistants avec redémarrage auto, forwarding d'agent (opt-in et averti) |
| Vault enrichi | Plusieurs coffres, export et import chiffrés, déverrouillage biométrique (Touch ID via Keychain `protected`) |
| Terminal | Broadcast d'input multi-hôtes (avec garde-fou), recherche dans le scrollback, liens cliquables |
| Plateformes | Thème clair |
| GUI native | Réévaluation de `gpui` si la GUI web atteint ses limites de performance |
| Intégrations | Teleport ou certificats éphémères (CA SSH d'entreprise) |

## Tableau de bord

| Phase | État | Tests | Revue sécu | Revue design |
|---|---|---|---|---|
| 0 Fondations | ✅ | 35 | couverte par la revue 1a | ✅ (captures TUI et GUI) |
| 1a Transport et auth | ✅ | 82 + interop OpenSSH (8 scénarios) | ✅ (voir ci-dessous) | statut live vérifié (tmux) |
| 1b Terminal TUI | ✅ | 103 + interop (shell réel) | ✅ (voir ci-dessous) | ✅ (tmux 80×24 et 110×28) |
| 1c Terminal GUI | ✅ | 106 + 3 GUI + interop | ✅ (voir ci-dessous) | ✅ (captures démo 3 thèmes + app réelle WebKitGTK) |
| 2 SFTP / tunnels | ✅ | 146 + 12 GUI + interop (11 scénarios + 1 Gio) | ✅ (voir ci-dessous) | ✅ (captures 3 thèmes, tmux 80×24 et 110×28, re-vérifiée) |
| 3 Clés / secrets / snippets | ✅ | 245 + 21 GUI + interop (12 scénarios + 1 Gio) | ✅ (voir ci-dessous) | ✅ (captures 3 thèmes, 880–1200 px, tmux 80×24 et 110×28) |
| 4 Finition | ✅ | 319 + 31 GUI + interop (12 scénarios) | ✅ (UIs + audit final MVP) | ✅ (captures 3 thèmes, 880–1400 px, tmux 80×24 et 110×28) |
| 5 SOCKS5, distribution, import Termius | ⏳ | | | |
| 6 Split panes, asciicast | ⏳ | | | |
| 7 Sync locale | ⏳ | | | |
| 8 Windows | ⏳ | | | |

## Revues de la phase 4 (2026-10-21)

Aucun constat CRITIQUE ni ÉLEVÉ ; tous les autres ont été traités, sauf mention.

| Revue | Constats | Principales suites |
|---|---|---|
| `agent-security` core 4a et release | 3 MOYEN, 5 FAIBLE | Signature macOS isolée (environnement `release`, outils Apple seulement), aucun cache en release, outillage AppImage épinglé et vérifié, actions épinglées par SHA, métadonnées validées en lecture, options sensibles (`SetEnv` masqué, commandes signalées), écritures plafonnées, liens et FIFO refusés |
| `agent-security` audit final MVP | 1 MOYEN, 1 FAIBLE | `cargo deny` remis au vert (apps `publish = false`, 5 avis triés par ID), log TUI hors de `/tmp` (`0600`, lien refusé), `SECURITY.md` avec les risques résiduels ; aucun interdit de `CLAUDE.md` §11 violé |
| `agent-design-review` | 1 BLOQUANT, 10 IMPORTANT | Barre de statut GUI qui perdait statut et latence au-delà de 1100 px ; couleurs des niveaux de log (INFO neutre), même grammaire du panneau de logs dans les deux UIs, aide `?` complète, avertissement `settings.toml` commun, « voir les logs » dans les états d'erreur, alias longs, chips de tags au clavier, `*` et `Shift-X` partagés |
| Mesures (`docs/PERF.md`) | 3 écarts | Chargement des gros `ssh_config` : 564 ms → 12 ms pour 2 000 hôtes ; curseur GUI fixe au repos (à re-mesurer) ; premier rendu GUI à ~1,1 s sous Xvfb sans GPU (environnement défavorable, à mesurer sur un vrai poste) |

Restent mineurs : un `draw` TUI à chaque sonde keepalive (invisible), « ↵ connect » dans la barre de statut alors que les détails
d'un hôte en échec disent « ↵ retry », l'aide TUI en 80×24 qui recouvre la barre de statut.

## Revues de la phase 3 (2026-10-21)

Aucun constat CRITIQUE ni ÉLEVÉ dans les cinq revues ; tous les autres ont été traités.

| Revue | Constats | Principales suites |
|---|---|---|
| `agent-security` clés/copy-id/agent | 2 MOYEN, 6 FAIBLE | Vérification sans certificat ni `none`, clés commentées ignorées, même compte, sonde fermée si abandonnée, codes d'erreur d'origine, `SysRng`, `~/.ssh` partagé refusé, bidi refusé |
| `agent-ssh-protocols` | 1 BLOQUANT, 10 IMPORTANT, 9 MINEUR | Certificats exclus de la preuve, script d'une ligne compatible csh, marqueur de résultat (rc bavards, forced command), canal toujours fermé, données après acceptation d'`exec`, retrait agent vérifié, `publickey` forcé |
| `agent-security` vault/snippets | 2 MOYEN, 6 FAIBLE | Verrou inter-processus, refus du collage multi-ligne sans bracketed paste, vault v2 (génération anti-retour, secrets en base64), bornes KDF 64–256 Mio, temporaires aléatoires, dossiers `0700`, snippets.toml borné et assaini, caractères invisibles refusés |
| `agent-security` UIs | 1 MOYEN, 4 FAIBLE | « Run it » sur `r` seulement après armement, passphrase redemandée après un refus de génération, `Debug` caviardé (TUI), durée d'agent hors bornes refusée, démo exclue du build de production |
| `agent-design-review` | 2 BLOQUANT, 8 IMPORTANT | Exception au déverrouillage du vault et toasts vides corrigés ; défilement TUI, sélection pendant les questions, vault toujours visible, pied sur une ligne, copy-id « Hide » + contexte dans les prompts, glossaire et raccourcis communs |

Restent mineurs : le toast en haut à droite recouvre l'indication « Ctrl-R » du titre de la vue Clés à 900 px, couleur
du point de statut du vault, libellé « Esc, then ↵ to connect » exact seulement depuis la liste d'hôtes.

## Revues de la phase 2 (2026-10-21)

Core revu par `agent-security` et `agent-ssh-protocols` (corrigé au commit `8385021` : fichiers partiels exclusifs,
noms hostiles, binds exposés, canaux non sollicités refusés, pair local lent coupé avant de geler la session).
Les UIs ont ensuite été revues séparément.

### `agent-security` (UIs) : 0 CRITIQUE/ÉLEVÉ, 2 MOYEN, 3 FAIBLE, tous traités
| Gravité | Constat | Suite donnée |
|---|---|---|
| MOYEN | GUI : la confirmation d'un bind exposé, déjà armée, se validait par un double-clic sur « Start » | Réarmement à l'apparition et après chaque frappe ou clic ailleurs, focus sur Cancel, liste inerte ; vérifié (double-clic, clic, clavier) |
| MOYEN | Dossier de téléchargement replié sur `$HOME` : un `.bashrc` du serveur pouvait remplacer le vrai | Jamais le home (`~/Downloads` créé en `0700`), aucun download n'écrase un fichier caché |
| FAIBLE | Jetons d'upload réutilisables vers n'importe quelle session | Liés au premier envoi, consommés par la réponse finale, expirés après 10 min |
| FAIBLE | Tauri annonce au webview les chemins déposés | Fuite de noms seulement ; test qui interdit `protocol-asset` et `tauri-plugin-fs`, ADR |
| FAIBLE | Chemins distants affichés bruts (bidi) dans le fil d'Ariane et la cible de dépôt | Assainis et isolés, renommage sans pré-remplissage d'un nom nettoyé |

### `agent-design-review` : 2 BLOQUANT, 7 IMPORTANT, tous traités (re-vérification : 1 régression, corrigée)
| Constat | Suite donnée |
|---|---|
| **BLOQUANT** : confirmation d'exposition GUI non réarmée, Tab menant à « Stop » | Voir la ligne sécurité ; la re-vérification a trouvé que le réarmement bloquait la validation elle-même : corrigé (`aria-disabled`, garde dans `expose()`) |
| **BLOQUANT** : session morte = erreur interne sans hint, « retry » inutile | Phase pendant la connexion, erreur + hint + `↵` reconnexion (deux UIs), message `session_ended` sans id, tunnels ciblant une session vivante |
| Suppression GUI : focus sur le bouton destructif | Focus sur « Keep », `y` / `n` |
| Actions de ligne focusables mais invisibles | Hors tabulation, visibles au focus |
| Clavier GUI incomplet, parité TUI/GUI | `r` `d` `x` `Ctrl-R` `k/o/s` « Overwrite » partout, palette complète, tunnels `↑↓` `↵` `Suppr` |
| Hints d'erreur des tunnels perdus ou tronqués | Message et hint séparés et repliés, effacés à la frappe suivante (deux UIs) |
| Aide `?` TUI tronquée à 80×24 | Notation unique, libellés courts, test de rendu 80×24 |
| Colonnes taille/date GUI désalignées | Grille en chiffres tabulaires |
| Re-vérification : reconnexion proposée après « clé changée » | Retirée dans les deux UIs (refus bloquant) |
| MINEUR traités | Accent réservé, cible de dépôt opaque, sélection du panneau inactif, dates compactes, PREFIX `f`/`p`, toasts dans le panneau, marqueur « exposed » TUI |
| MINEUR ouverts | Tailles en px dans FilesPanel/TunnelsDialog (tokens `--size-*` à créer), pulse en `box-shadow`, accent Travertine proche de l'erreur, message de parse pour `-L` vide, dossier distant non restauré après reconnexion |

## Revues de la phase 1c (2026-10-20)

Vérifié sur l'app réelle (WebKitGTK sous Xvfb, sshd local) : connexion directe et via bastion, passphrase avec
nouvel essai, clé modifiée (alerte), `vim`, `top`, resize, flot `base64 /dev/urandom` (mémoire stable, `Ctrl-C`
immédiat), 1 M lignes en 1,9 s, 3 000 caractères tapés dans l'ordre, rechargement de page qui ferme les sessions,
dernier onglet qui déconnecte.

### `agent-security` : 0 CRITIQUE/ÉLEVÉ, 2 MOYEN, 7 FAIBLE, tous traités
| Gravité | Constat | Suite donnée |
|---|---|---|
| MOYEN | Un utilisateur qui tape encore (autre onglet) pouvait valider une clé hôte avec `y` ou envoyer du texte comme mot de passe | Armement après 400 ms sans frappe, `y` 150 ms après l'armement |
| MOYEN | Entrée terminal réordonnable (flush parallèles, commandes async indépendantes) | Boucle d'envoi unique, vérifiée (3 000 caractères) |
| FAIBLE | Acks non plafonnés, compteurs partagés entre reconnexions | Plafond aux octets envoyés + test, compteurs par ouverture |
| FAIBLE | Liens OSC 8 avec le gestionnaire par défaut | Liens inertes jusqu'au post-MVP |
| FAIBLE | Textes keyboard-interactive non assainis (phishing de passphrase) | Assainis, plafonnés, étiquetés « Message from … » |
| FAIBLE | Commande `ssh-keygen -R` mal quotée, fichier ignoré | Quotage POSIX, `-f` si autre fichier, copie seulement pour un nom simple |
| FAIBLE | Réponses du terminal perdues avant l'id | Mises en file puis envoyées |
| FAIBLE | Capabilities trop larges, drag de fenêtre absent | Réduites au strict nécessaire, drag autorisé |
| FAIBLE | Sortie sans fermeture propre | `prevent_exit` + `shutdown_and_wait` (500 ms) |

### `agent-ssh-protocols` : 1 BLOQUANT, 6 IMPORTANT, 6 MINEUR, tous traités
| Constat | Suite donnée |
|---|---|
| **BLOQUANT** : fermer un onglet pendant la connexion n'annulait rien (prompts orphelins, `known_hosts` encore écrivable) | `disconnect` annule le jeton, prompts retirés par garde `Drop` + test |
| Rechargement de page : PTY, sessions et forwarders laissés vivants | `on_page_load` → `close_all` + `disconnect_all`, vérifié sur l'app réelle |
| Ordre d'entrée, taille PTY figée pendant l'auth, fin de terminal perdue avant l'id | Boucle unique, taille réconciliée après ouverture (pixels inclus), fins précoces mémorisées |
| `macOptionIsMeta` bloquant `|[]{}~` sur claviers Mac non US ; raccourcis sur `e.code` (AZERTY) | `Option` normal, lettres via `e.key` |
| MINEUR : `Ctrl-Shift-2/6` volés à emacs/vim, sortie non propre, emoji coupé, tampon 64 Mio, reconnexions créant des sessions | `Alt-n`, sortie propre, découpe sûre, `MAX_INPUT` 64 Kio, réutilisation de la session vivante |

### `agent-design-review` : 1 BLOQUANT, 8 IMPORTANT, tous traités
| Constat | Suite donnée |
|---|---|
| **BLOQUANT** : transitions Svelte ignorant `prefers-reduced-motion`, durées en dur | `dur()` lit les tokens et renvoie 0 en mouvement réduit |
| Onglet actif masqué quand la barre déborde, anneau de focus rogné | Défilement après mise en page, fondus de bord, Hosts épinglé, anneau intérieur, tabindex itinérant |
| Statut contradictoire après `exit` | Grammaire unique : couleur = session, remplissage = shell attaché (onglet, barre, bandeau) |
| Empreintes coupées en plein milieu | Une ligne, colonne de libellés `max-content` |
| En-tête des détails cassé à 900 px | `flex-wrap`, badge `nowrap`, padding réduit |
| « Retry » primaire sur clé modifiée | `Close` primaire, `Retry` secondaire, `Show details` rouvre l'alerte |
| Basalt : accent = `status-connecting` | Nouvelle teinte + test ΔE sur les trois thèmes (a aussi corrigé Obsidian) |
| Polices non embarquées | `@fontsource`, xterm créé après chargement |
| Polish : ombres/tailles/pulse en dur, palette, toast, `Esc`, focus du champ, badge démo | Tokens, palette complétée, toast avec ton et icône, `Esc` → hôtes, anneau 2 px, badge dans la barre |

## Revues de la phase 1b (2026-10-20)

### `agent-security` : 1 ÉLEVÉ, 2 MOYEN, 2 FAIBLE, tous traités
| Gravité | Constat | Suite donnée |
|---|---|---|
| ÉLEVÉ | Un collage pendant un dialogue partait vers le shell ou la recherche | Corrigé : le collage va au dialogue ouvert, jamais à une réponse de clé hôte |
| MOYEN | Une frappe en vol (`y`, texte du shell) pouvait répondre à un dialogue qui venait de s'ouvrir | Corrigé : les dialogues ignorent les touches pendant 400 ms |
| MOYEN | Le bracketed paste pouvait être contourné en coupant le marqueur de fin | Corrigé : tous les ESC/C1 sont retirés du collage |
| FAIBLE | Titre distant (OSC) capable d'usurper un autre hôte (bidi, longueur) | Corrigé : titre assaini, tronqué, toujours affiché après l'alias |
| FAIBLE | Champs secrets réalloués sans effacement | Corrigé : pré-dimensionnés, plafonnés, effacés après transmission |

### `agent-ssh-protocols` : 2 BLOQUANT, tous traités
| Constat | Suite donnée |
|---|---|
| **BLOQUANT** : `Ctrl-C` bloqué derrière un flot de sortie | Corrigé : tâches lecteur et écrivain séparées + test (flot + `Ctrl-C`) |
| **BLOQUANT** : un écrivain en attente de fenêtre gelait la boucle russh (toute la connexion) | Corrigé par la même séparation ; resize via `watch`, fermeture via jeton |
| Canal laissé à moitié ouvert après un échec d'ouverture | Corrigé : EOF + CLOSE sur tous les chemins, ouverture hors de la boucle de supervision |
| Modes PTY incomplets, `exit-signal` ignoré, EOF sans CLOSE | Corrigé (`ICRNL`, vitesses, `Exited { signal }`) + tests |
| Backpressure par canal impossible (russh ajuste la fenêtre à la réception) | Limite upstream consignée dans STACK ; mémoire bornée, débit régulé au niveau TCP |

### `agent-design-review` : 2 BLOQUANT, 9 IMPORTANT, tous traités
| Constat | Suite donnée |
|---|---|
| **BLOQUANT** : en-tête d'onglets qui déborde à 80 colonnes et masque l'onglet actif | Corrigé : fenêtre glissante autour de l'onglet actif (`‹+n` / `+n›`), alias tronqués, thème affiché seulement s'il tient |
| **BLOQUANT** : la pastille de statut perdait sa couleur sur la ligne sélectionnée | Corrigé : la sélection ne change plus que le fond |
| Dialogues : titre sans contexte, actions non hiérarchisées | Corrigé : « quoi · hôte », actions à droite, action principale en dernier et accentuée |
| Mot de passe faux redemandé sans explication | Corrigé : `attempt` / `attempts` dans les prompts, « Incorrect …, try again (2/3) » |
| Annuler le mot de passe affichait une erreur | Corrigé : fin de session `Closed { UserRequested }`, sans toast |
| Alerte de clé modifiée peu lisible | Corrigé : bandeau d'erreur, empreinte reçue en gras, commande `ssh-keygen -R` exacte |
| Échec : message et hint mélangés | Corrigé : message, hint et remède sur des lignes distinctes (détails et toast) |
| Barre de statut : libellés de touches incohérents, PREFIX peu visible | Corrigé : libellés uniformes, pastille PREFIX inversée, `● alias · latence · titre` à droite |
| Préfixe absent de la liste des hôtes, alias de touches redondants | Corrigé : `Ctrl-Space` partout, alias `l p c x k` retirés |
| Refus volontaire d'une clé affiché comme une erreur | Corrigé : statut neutre « Refused » |
| Bannière de fin de shell : code ≠ 0 non signalé | Corrigé : couleur d'avertissement |

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
