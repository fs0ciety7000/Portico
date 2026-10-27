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
Couvert aussi par le scénario d'interop 12. En GUI : parcours complet vérifié dans l'app de bureau réelle par l'E2E (`pnpm e2e`, 2026-10-22).

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
| 7 | Copie de clé publique (`ssh-copy-id`) | ✅ | ✅ | Réel en TUI, en interop et dans l'E2E de l'app de bureau |
| 8 | Port forwarding local et distant | ✅ | ✅ | Interop scénarios 10 et 11, `nc` sur un tunnel exposé |
| 9 | Sessions récentes et favorites, onglets multi-sessions | ✅ | ✅ | Tests d'intégration (récents), rendu 80×24, captures |
| 10 | Snippets (CRUD, portée, placeholders, exécution) | ✅ | ✅ | Tests de rendu sûr, exécution réelle sous `sh` en TUI |
| 11 | Statut de connexion et latence | ✅ | ✅ | Barre de statut vérifiée de 880 à 1400 px, tmux 80×24 |
| 12 | 3 thèmes sombres et raccourcis documentés | ✅ | ✅ | Tests de contraste WCAG, `KEYBINDINGS.md` généré et testé |
| 13 | Secrets dans le keyring (vault chiffré en repli) | ✅ | ✅ | Vault v2 testé (verrou, anti-retour), déverrouillage au démarrage |
| 14 | Mode debug : logs de connexion par session | ✅ | ✅ | Panneau de logs, sessions terminées conservées |

**Vérifié dans l'app de bureau réelle** (2026-10-22) : les parcours des phases 3 et 4 de la GUI (vault, clés, agent, copy-id
et sa vérification, connexion par la seule clé, snippets et refus du collage sous `sh`, favoris et édition, récents, logs et
configuration effective, thème, SFTP, tunnel `-L`, redémarrage avec déverrouillage) ont été parcourus dans l'app Tauri sous
WebKitGTK contre un `sshd` OpenSSH, et un E2E rejouable les couvre (`pnpm e2e`, voir [TESTING.md](TESTING.md)). Reste à faire
avant la première release publique : la re-mesure du CPU au repos de la GUI (`bench/run-all.sh`).

---

## Après le MVP : phases 5 à 8 (ordre validé le 2026-10-21)

Mêmes règles que pour le MVP : une phase n'est close qu'après `agent-security` (et `agent-design-review` si elle
touche l'UI), et tout choix structurant passe par une ADR dans `docs/STACK.md`.

### Phase 5 : gains rapides et distribution ✅ *(code livré ; sortie à confirmer sur une vraie release et un vrai export)* *(Opus pour le core, Sonnet pour l'UI)*
- `-D` (SOCKS5) dans `forward/` : SOCKS5 sans authentification (CONNECT, IPv4/IPv6/domaine), écoute sur `localhost`
  par défaut, écoute exposée sur confirmation armée, panneau Tunnels des deux UIs ; `DynamicForward` de `ssh_config` proposé
  ✅ *(livré : core, TUI, GUI ; `curl --socks5-hostname` vérifié contre OpenSSH, interop scénario 13 ; GUI vérifiée en mode démo
  seulement)*
- Distribution : tap **Homebrew** (formula TUI, cask GUI) et paquet **AUR** `-bin`, générés depuis les artefacts et
  checksums de la release, mis à jour par le workflow de release
  ✅ *(gabarits et scripts de rendu dans `packaging/`, job sur release publiée, voir `docs/RELEASE.md` ; à exercer sur une
  vraie release : dépôt du tap, compte AUR et secrets du propriétaire)*
- Import **Termius** (après vérification du format d'export réel) : génère un fichier de config dans le dossier de Portico
  (jamais dans `~/.ssh`) et indique la ligne `Include` à ajouter soi-même ; tags et groupes importés dans `hosts.toml`.
  Aucun secret importé en clair
  ✅ *(livré : core `import`, `portico import termius FILE [--prefix] [--apply]`, palette GUI « Import from Termius… »
  avec aperçu, voir `docs/IMPORT.md`. Termius n'a pas d'export officiel des hôtes : formats lus = CSV du modèle d'import
  (en-tête vu dans des sources non officielles, lu avec tolérance), `hosts.json`/`credentials.csv` de
  `termius-local-export`, sortie de `termius export-ssh-config`. Vérifié sur fixtures synthétiques seulement ; à
  confirmer sur un vrai export. GUI vérifiée en mode démo seulement)*

**Sortie** : `curl --socks5-hostname localhost:1080` passe par un hôte via Portico (les deux UIs) ; `brew install` et
l'installation depuis l'AUR fonctionnent sur une release taguée ; un export Termius de test donne les mêmes hôtes dans Portico.

État de la sortie (2026-10-22) : `curl --socks5-hostname` ✅ (interop OpenSSH, TUI ; GUI en mode démo). En attente du
propriétaire : `brew install` et AUR (tap, compte AUR, secrets, première release taguée) ; import d'un vrai export Termius
(fixtures synthétiques seulement à ce jour).

### Phase 6 : terminal ✅ *(décisions : `docs/STACK.md`, 2026-10-22, split panes et enregistrement)*
- **Split panes** : divisions horizontales et verticales dans un onglet, focus et redimensionnement au clavier, chaque
  panneau étant un terminal indépendant (même session ou autre hôte) ; TUI et GUI
- **Enregistrement asciicast v2** (ADR du 2026-10-22 : exception encadrée à l'interdit « contenu de terminal en clair ») :
  opt-in explicite par terminal, indicateur « REC » toujours visible, sortie seulement (les saisies, donc les mots de passe,
  ne sont pas enregistrées), fichiers `0600`, avertissement ; relecture dans les deux UIs. Chiffrement reporté au thème
  « Vault enrichi » (voir l'ADR)

**Sortie** : quatre panneaux sur deux hôtes utilisables au clavier seul ; une session enregistrée se relit à l'identique.

### Phase 11 : synchronisation chiffrée par dossier ⏳ *(ADR du 2026-10-27 ; remplace la phase 7 abandonnée le 2026-10-22 ; conception, décisions du propriétaire et revue `agent-security` : `docs/SYNC.md`)* *(Opus)*
Hôtes de `hosts.conf`, métadonnées, snippets et réglages en liste blanche (puis, en S2, mots de passe liés en opt-in) partagés
entre les machines de l'utilisateur via un dossier qu'il synchronise déjà (Dropbox, iCloud Drive, OneDrive, Syncthing, git…).
Portico ne fait aucun appel réseau, le dossier ne contient que du chiffré, les conflits sont montrés et jamais résolus en
silence. Jamais synchronisés : clés privées, `known_hosts`, `~/.ssh/config`, enregistrements, logs, consentement aux mises à jour.

- **S0, repérage** : comportements réels des fournisseurs (renommages, copies en conflit, fichiers temporaires, placeholders)
  consignés dans `docs/SYNC.md`, fixtures du fournisseur simulé
- **S1, sync sans secret**
  - **S1a, fondations pures** ✅ *(2026-10-27)* : `portico-core/src/sync/` sans E/S ni moteur : Argon2id à bornes propres,
    HKDF, XChaCha20-Poly1305, AAD canonique, MAC en temps constant, remplissage ; `bundle.toml` (créer, ouvrir, changer et
    réinitialiser la passphrase, roster scellé) ; instantanés et réplica scellés ; registres multi-valeurs à contexte global,
    pierres tombales, conflits, registres génériques (compatibilité ascendante) ; détection de retour arrière, fourche, rétrogradation et état restauré ;
    générateur EFF et minimum de force imposé. Correctifs de la revue de sécurité de l'implémentation (empreintes du contenu
    authentifié, fusion transactionnelle, contextes de tiers différés, registres génériques, crédit diceware réservé aux
    passphrases générées, chemins d'identité stricts). 93 tests dont les lois de fusion en tests de propriétés
  - **S1b, moteur** ✅ *(2026-10-27)* : `FolderIo` et racine sûre (liens et jonctions refusés, dossiers privés exclus,
    limites de listage et d'octets), projection des fichiers locaux (éditions à la main, F7), application validée
    (quarantaine, masquage d'alias, changements de destination retenus jusqu'à confirmation sur l'appareil, F1 ; snippets
    reçus refusés au lancement avant confirmation, F8), état local scellé, passe sûre en cas de crash (fichier propre lu
    avant toute écriture, contextes de tiers différés, fusion attribuée, collecte stable), thread `portico-sync` et tâche
    de fond, statut et événements, API `Engine::sync_*` (create / join à jeton unique / unlock / resolve / confirm /
    change / reset / disable…), clé de données dans le trousseau. Correctifs de la revue de sécurité S1b (15 constats,
    aucun critique ni élevé) : confirmations liées au contenu montré et à usage unique, `confirm.toml` barrière
    d'exécution écrite avant le snippet et conservée à la désactivation, hôtes locaux non synchronisables jamais écrasés,
    suppressions différées pendant une suspension, écritures du dossier ancrées sur descripteurs (Unix), TUI branchée
    sur `snippet_input_for`. 46 tests de plus (139 au total pour la sync), dont 29 scénarios multi-moteurs sur un
    fournisseur simulé, plus un test TUI de bout en bout
  - **S1c, interfaces** : assistant, barre de statut, panneau Sync, conflits, issues, badges, dans la TUI et la GUI
- **S2, rotation puis mots de passe** : rotation des clés et « oublier un appareil » **d'abord** ; ensuite mots de passe
  **liés** seulement (jamais les non liés), opt-in au niveau du bundle et décoché par défaut, envoyés sur B seulement si B
  vérifie la même clé hôte
- **S3, plus tard** : emplacements de clé par appareil (rotation sans ressaisir la passphrase), signatures Ed25519, exclusions
  par réglage, options à décider

**Sortie S1** : deux machines partagent hôtes, métadonnées, snippets et thème via un dossier synchronisé ; une édition hors ligne
sur les deux devient un conflit montré et résolu dans les deux UIs ; un changement de destination reçu n'est écrit qu'après
confirmation sur l'appareil ; une ancienne copie remise dans le dossier est refusée (`sync_rollback`).
**Sortie S2** : exercice d'appareil perdu (oubli + rotation depuis un appareil restant : l'appareil oublié ne lit plus les
nouvelles données) ; un mot de passe mémorisé sur A n'est proposé sur B que pour la même clé hôte vérifiée.
**Sortie S3** : rotation reçue par tous les appareils sans ressaisie de la passphrase.
Revues : `agent-security` après crypto + format + dossier, après S2 et à la clôture (avec `SECURITY.md`) ;
`agent-design-review` sur l'assistant, la barre de statut, les conflits et les issues (4 thèmes, 80×24, 880–1200 px).

### Phase 6b : hôtes gérés par Portico ✅ *(ADR du 2026-10-22, « Hôtes créés dans Portico »)*
- Créer, modifier, dupliquer et supprimer des hôtes depuis la TUI et la GUI : alias, `HostName`, `User`, `Port`,
  clé (`IdentityFile` choisie parmi les clés locales), `ProxyJump` (hôte existant), plus tags, groupe, favori et notes
- Stockage dans `<config_dir>/hosts.conf` (fragment ssh_config de Portico), lu directement par Portico ; ligne `Include`
  proposée pour que `ssh`, `scp` et `git` voient aussi ces hôtes. Les hôtes de `~/.ssh/config` restent en lecture seule
- Mot de passe optionnel mémorisé dans le keyring (ou le vault), jamais dans le fichier

**Sortie** : un hôte créé dans Portico s'y connecte aussitôt, sans rien éditer à la main ; une fois l'`Include` ajouté,
`ssh <alias>` fonctionne à l'identique.

Ajouté pendant la phase, à la demande du propriétaire : **import Termix** (export JSON) et **import de clés privées sur
sélection explicite** (`~/.ssh/portico/`, jamais d'écrasement) ; les imports Termius et Termix deviennent des hôtes
Portico modifiables (ADR « Import Termix et imports vers les hôtes Portico »).

État de la sortie (2026-10-22) : vérifiée. Hôte créé puis connexion immédiate (TUI contre un vrai sshd, E2E bureau),
`ssh -G` d'OpenSSH résout le même `HostName`/`User`/`Port` que Portico ; import Termix avec une clé cochée puis connexion
avec cette clé (E2E bureau).

### Phase 9 : confort du terminal ✅ *(ordre validé le 2026-10-22 : phase 9, puis thème clair, puis Windows)*
- **Recherche dans l'historique**, TUI (`Ctrl-Space /`) et GUI (`⌘F` / `Ctrl-Alt-F`) : texte littéral, casse intelligente,
  compteur « 3 of 17 » numéroté depuis la correspondance la plus récente, « No match », « 999+ » ; rien n'atteint le
  serveur tant que la barre est ouverte ; la vue ne bouge pas quand la sortie continue. 2 à 7 ms (TUI) et 1 à 6 ms
  (GUI) par frappe sur 10 000 lignes
- **Liens** (GUI) : http(s) seulement, `⌘`/`Ctrl`-clic, toujours confirmés (« Open this link? », hôte mis en évidence,
  avertissement quand le texte OSC 8 nomme un autre hôte, Cancel par défaut dans ce cas), ouverture par `open_url`
  côté Rust (revalidation, aucun shell). Dans la TUI, le terminal hôte gère déjà les URL visibles
- **Menu contextuel** du terminal (GUI) et option **Copy on select** (désactivée par défaut), nouvelle fenêtre Settings

- **Vérification des mises à jour, opt-in** (seule exception réseau hors SSH, ADR du 2026-10-23) : question unique au
  premier lancement, au plus une requête par jour vers la dernière release GitHub (rien d'autre envoyé que la version
  dans le User-Agent), réglage relu avant chaque requête, badge « x.y.z available » et commande de mise à jour adaptée
  à l'installation ; jamais de téléchargement ni d'installation

**Sortie** : vérifiée (E2E bureau 22/22, captures TUI 80×24 et GUI 3 thèmes).

### Phase 10 : thème clair ✅ *(décision du propriétaire, 2026-10-24 ; `CLAUDE.md` §3 amendé)*
- **Marble** : surfaces marbre chaud (jamais blanc pur), texte graphite, un seul accent bronze, élévation inversée
  (dialogues au plan le plus clair), statuts réaccordés pour le clair, palette ANSI claire ; contraste testé dans les
  quatre thèmes (texte primaire AAA, secondaire et muted AA, statuts jamais confondus avec l'accent : Δteinte ≥ 15°
  ou ΔL ≥ 10). Couleurs choisies par le serveur foncées juste assez pour 4,5:1 sur fond clair
- **Auto** : Marble quand le système est clair, le thème sombre choisi sinon (`theme = "auto"`, `dark_theme`) ; GUI en
  direct (`prefers-color-scheme`, sans flash au démarrage), TUI par requête OSC 11 au démarrage (seulement en Auto,
  réponse tardive jamais lue comme des touches), repli `COLORFGBG` puis sombre. `t` : Obsidian → Basalt → Travertine
  Night → Marble → Auto

**Sortie** : vérifiée (E2E bureau 22/22, captures GUI démo + WebKitGTK, TUI 80×24).

### Phase 8 : Windows ✅ *(revues `agent-security` ×2 et `agent-ssh-protocols` ; test manuel Windows 11 du propriétaire)*
- Gestionnaire d'identifiants Windows (`keyring`, persistance *Local*, jamais itinérante), agent OpenSSH sur named pipe
  (identité du serveur vérifiée : service SYSTEM ou l'utilisateur), WebView2, chemins `%USERPROFILE%\.ssh`,
  données dans `%LOCALAPPDATA%`
- **Modèle de sécurité des fichiers refait avec des DACL** (propriétaire seul, posées sur le handle avant écriture,
  points d'analyse refusés) dans `crates/portico-winsec`, seul crate autorisé à `unsafe` ; `known_hosts` système de
  ProgramData utilisé seulement s'il appartient à la machine ; noms Windows invalides refusés ; durcissement (rapports
  WER sans tas, DLL chargées depuis System32)
- Distribution : zip TUI et installeur NSIS par utilisateur **non signés** (signature reportée, ADR), Scoop (bucket du
  propriétaire, PR automatique si le jeton existe) et manifestes winget rendus pour une PR manuelle
- Parsing `ssh_config` aligné sur `argv_split` d'OpenSSH (guillemets et échappements), agent muet abandonné après 5 s

**Sortie** : vérifiée (CI `windows-latest` verte, release à blanc avec installation/désinstallation testée sur le
runner, test manuel concluant du propriétaire sous Windows 11 le 2026-10-27).

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
| Plateformes | Thèmes clairs supplémentaires si demandés |
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
| 5 SOCKS5, distribution, import Termius | ✅ (sortie partielle, voir phase 5) | 384 + 32 GUI + interop (13 scénarios) + E2E bureau 13/13 | ✅ (voir ci-dessous) | ✅ (import : captures 3 thèmes, 880–1200 px) |
| 6 Split panes, asciicast | ✅ | 548 + 50 GUI + interop (13 scénarios) + E2E bureau 15/15 | ✅ (moteur + clôture, voir ci-dessous) | ✅ (panneaux + enregistrement, 2 passes, 3 thèmes, 80×24 / 880–1200 px) |
| 6b Hôtes Portico, import Termix | ✅ | 611 + 61 GUI + interop (13 scénarios) + E2E bureau 19/19 | ✅ (2 revues, voir ci-dessous) | ✅ (captures 3 thèmes, 80×24 / 880–1200 px, CLI) |
| 7 Sync locale | ❌ abandonnée (rouverte en phase 11) | | | |
| 8 Windows | ✅ | 780 + 95 GUI + interop (13 scénarios) + CI windows-latest + test manuel Windows 11 | ✅ (2 audits, voir ci-dessous) | GUI Windows : barre native, raccourcis WebView2 neutralisés |
| 9 Confort du terminal, mises à jour | ✅ | 736 + 89 GUI + interop (13 scénarios) + E2E bureau 22/22 | ✅ (voir ci-dessous) | ✅ (2 passes, 3 thèmes, 80×24 / 880–1200 px) |
| 10 Thème clair Marble, Auto | ✅ | 760 + 89 GUI + interop (13 scénarios) + E2E bureau 22/22 | ✅ (voir ci-dessous) | ✅ (4 thèmes, 80×24 / 880–1200 px) |
| 11 Sync chiffrée par dossier | ⏳ S1a (fondations) et S1b (moteur) livrés ; S1c, S2, S3 à venir | 139 (S1a 93, dont 6 tests de propriétés ; S1b 46) | revue de conception ✅ (F1–F18, `docs/SYNC.md` §12) ; revue S1a ✅ ; revue S1b ✅ (15 constats corrigés) | — |

## Revues de la phase 8 (2026-10-25)

| Revue | Constats | Principales suites |
|---|---|---|
| `agent-security` audit Windows | 2 ÉLEVÉ, 1 MOYEN, 4 FAIBLE | Pipe d'agent squattable (clé privée envoyée à un autre compte) : identité du serveur vérifiée ; `known_hosts` système créable par tout compte : utilisé seulement s'il appartient à la machine, ProgramData par dossier connu ; secrets et données non itinérants ; noms de clés réservés, nom de machine, durcissement WER/DLL, COM0/LPT0 |
| `agent-ssh-protocols` portage Windows | 2 IMPORTANT, 8 MINEUR | Délais bornés pour l'agent (repli sur clés et mot de passe), lexer `ssh_config` = `argv_split` d'OpenSSH, indications du service agent, `icacls` exact, utilisateur local en minuscules, `\\?\pipe\` accepté |
| `agent-security` re-revue | 0 CRITIQUE/ÉLEVÉ, 4 FAIBLE | Repli du contrôle d'agent refusé si un autre compte peut créer une instance du pipe ; contrôle de désinstallation non vide ; filtrage cohérent des `known_hosts` globaux ; borne de copie du SID |
| CI `windows-latest` | 4 échecs réels corrigés | Propriétaire Administrateurs quand le processus est élevé (le propriétaire est désormais l'utilisateur), lecture de la sécurité du pipe comme objet noyau, test de latence, test `known_hosts` |

## Revues de la phase 10 (2026-10-24)

Aucun constat CRITIQUE ni ÉLEVÉ ; tous les constats ont été traités.

| Revue | Constats | Principales suites |
|---|---|---|
| `agent-security` Marble, Auto | 1 MOYEN, 1 FAIBLE | Une réponse OSC 11 tardive (terminal lent, SSH) n'est plus lue comme des touches : lecture prolongée jusqu'à DA1 dès qu'un octet arrive (1 s max), puis filtre de 5 s sur les touches égarées ; requête seulement en Auto. Vérifié : lecture bornée, rien de journalisé, `set_window_theme` validé, capabilities et CSP inchangées |
| `agent-design-review` Marble, Auto | 2 IMPORTANT, 7 MINEUR | Avertissement Marble éloigné de l'accent bronze (et règle testée dans les quatre thèmes) ; captures régénérées ; pastilles de thème (fond + cœur d'accent, Auto en deux moitiés) ; `t` affiche le thème dans la GUI comme dans la TUI ; texte muted Marble AA ; voile sous les modales TUI en clair ; ellipse sur `user@host` |

## Revues de la phase 9 (2026-10-23)

Aucun constat CRITIQUE, ÉLEVÉ ni MOYEN ; tous les constats ont été traités.

| Revue | Constats | Principales suites |
|---|---|---|
| `agent-security` liens, recherche, menu, copy on select | 1 FAIBLE | `open_url` refuse aussi les caractères de format invisibles (bidi, largeur nulle), comme la vue ; vérifié : aucune injection d'option ou de schéma, seul l'hôte est journalisé, capabilities et CSP inchangées, `allowProposedApi` sans surface distante |
| `agent-security` vérification des mises à jour | 1 MOYEN, 2 FAIBLE | Réglage relu sur disque avant chaque requête (une autre instance ou une édition manuelle qui coupe la vérification est respectée, testé sur 72 h) ; cas d'URL piégées ajoutés aux tests ; variables de proxy documentées. Vérifié : aucune requête sans opt-in, TLS vérifié, https seulement, 10 s, 1 Mio, en-têtes exacts |
| `agent-design-review` mises à jour | 1 BLOQUANT, 4 IMPORTANT | `↵` sur un bouton focalisé agit sur ce bouton au lieu de connecter l'hôte sélectionné (bug antérieur) ; `U` ouvre la vue Updates, l'activation exige `y` ; bannière et dialogue sans accent qui pousse au oui ; question TUI plus discrète ; titre de palette trouvable par « update » |
| `agent-design-review` clôture 9 | 2 BLOQUANT, 6 IMPORTANT, 5 MINEUR | Correspondance courante en `text-on-accent` (7,5 à 9,8:1) ; barre TUI qui possède sa ligne et reste en bas ; `search-match` distinct de la sélection (ΔE ≥ 10, testé) et texte forcé en `text-primary` ; ↑/↓ et aides visibles dans la GUI ; Cancel par défaut pour un lien trompeur ; groupe radio des thèmes ; `Ctrl-Alt-,` pour Settings sous Linux ; un seul nom « Search scrollback » ; tokens `size-control`, `size-menu` |

## Revues de la phase 6b (2026-10-22)

Aucun constat CRITIQUE ni ÉLEVÉ ; tous les autres ont été traités, sauf mention.

| Revue | Constats | Principales suites |
|---|---|---|
| `agent-security` hôtes Portico, import Termix, clés | 3 MOYEN, 7 FAIBLE | Un réimport ne redirige plus un alias en silence (ancienne destination montrée, acceptation par hôte, clé non reprise) ; paramètres KDF des PEM chiffrées bornés avant tout déchiffrement (scrypt, PBKDF2, mémoire, nombre par plan) ; mot de passe mémorisé retenu quand la clé hôte est acceptée automatiquement (`accept-new`/`no`) et annoncé dans le prompt TOFU ; mot de passe refusé jamais lié et oublié ; index des empreintes liées ; empreintes non vérifiables jamais réutilisées ; noms de fichiers de clés sans suffixe OpenSSH ; retour arrière du mot de passe sur échec d'écriture ; secrets JSON décodés dans un tampon effaçable ; alias courts (`localhost`, hôtes de rebond) préfixés. Risques résiduels 13–15 dans `SECURITY.md` |
| `agent-design-review` clôture 6b | 2 BLOQUANT, 9 IMPORTANT | Mot de passe sans vault : création ou déverrouillage enchaîné puis enregistrement relancé (`vault_missing` distinct de `vault_locked`) ; « Delete <fichier> now » en tête du rapport ; mot de passe mémorisé affiché (présence seulement) et texte exact quand la destination change ; provenance « Portico · from Termix » ; comptes de secrets cohérents ; touches, titres, textes de suppression et TOFU (`host:port`, compte nommé) identiques dans les deux UIs ; tokens de taille ajoutés |

## Revues de la phase 6 (2026-10-22)

Aucun constat CRITIQUE restant ; tous les autres ont été traités. Sortie vérifiée : quatre panneaux sur deux hôtes au clavier
(TUI, tmux contre un vrai sshd ; GUI, mode démo et app de bureau) ; une session enregistrée sur un vrai sshd se relit
(E2E bureau : fichier v2, `0600`, contenu rejoué dans le lecteur).

| Revue | Constats | Principales suites |
|---|---|---|
| `agent-security` moteur d'enregistrement | 1 ÉLEVÉ, 7 FAIBLE | Contournements du filtre de relecture (ESC + C0/DEL, C1 en UTF-8) fermés ; invariant « aucun C1, tout ESC suivi de 0x20–0x7E » vérifié par un fuzz différentiel contre vte et un modèle de la table de xterm.js ; noms valides, démarrage annulable, thread d'écriture dédié, troncature aux lignes complètes, dimensions bornées, dossier vérifié par descripteur (propriétaire, `openat`), suppression refusée en cours d'écriture, titres sans caractères de format |
| `agent-design-review` panneaux | 2 BLOQUANT, 7 IMPORTANT | Séparateurs TUI en `border-subtle`, statut toujours visible avec un alias long, `⌘W` ferme le panneau et `⌘⇧W` l'onglet, raccourcis Linux hors `Ctrl-Alt` (espaces de travail GNOME), mode panneau répétable (TUI), refus sur une ligne, vocabulaire commun |
| `agent-security` clôture | 1 MOYEN, 2 FAIBLE | Un terminal qui s'ouvre tard ne vole plus le focus (onglet créé et activé à l'action) ; REC ne reste plus figé après une resynchronisation ; une lecture ouverte pendant un rechargement est refusée (époque) ; collage limité au panneau actif |
| `agent-design-review` clôture | 1 BLOQUANT, 10 IMPORTANT | Texte de consentement exact (« les commandes tapées sont renvoyées en écho, donc enregistrées ») ; seul `y` confirme dans les deux UIs ; « Recording saved » à l'arrêt ; suppression d'un enregistrement en cours refusée d'emblée ; dates locales et durées en horloge communes ; lecteur : rejouer, −5 s, début, statut visible ; REC sur l'onglet dès qu'un panneau enregistre ; tokens `--size-bar`, `--accent-selection` |

## Revues de la phase 5 (2026-10-22)

Aucun constat CRITIQUE ni ÉLEVÉ ; tous les autres ont été traités, sauf mention.

| Revue | Constats | Principales suites |
|---|---|---|
| `agent-ssh-protocols` `-D` | 4 IMPORTANT, 4 MINOR | Canaux `direct-tcpip` confirmés en retard ou orphelins toujours fermés (`-L` et `-D`), budget d'octets de 64 Mio par session au lieu d'une coupure à 2 s (un flux en pause survit), refus SOCKS explicite quand le tunnel est plein, IPv6 entre crochets dans les logs, syntaxe `bind/port`, interop scénario 13 renforcé (SOCKS4a, ATYP IPv4, pipelining, arrêt) |
| `agent-security` phase 5 | 2 MOYEN, 5 FAIBLE | E2E : `tauri-driver` sur ports aléatoires (RCE cross-origin reproduite sur 4444), reviewers obligatoires et tags `v*` protégés pour `distribution`, confirmation « open proxy » pour un `-D` exposé, logs limités en rafale, destinations SOCKS réussies non journalisées, `SECURITY.md` risques 11 et 12. Non fait : attestation de provenance des artefacts (couverte par l'approbation obligatoire) |
| `agent-security` import Termius | 3 MOYEN, 4 FAIBLE | Copie utilisateur après l'`Include` détectée au réimport, ligne CSV décalée sautée (aucun mot de passe déplacé), alias en forme de nom d'hôte préfixé `termius-`, ligne `Include` recalculée à l'application, avis bornés, application tout-ou-rien, hygiène `Zeroizing` |
| `agent-design-review` import Termius | 2 BLOQUANT, 6 IMPORTANT | Import vide refusé et remplacement annoncé (« Replaces the N hosts »), `Include` détecté par directive, `↵` et focus dans le dialogue, colonnes alignées, option de préfixe visible, étape `Include` mise en avant, kbd dans les boutons, état vide, aide TUI, CLI plus lisible |

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
