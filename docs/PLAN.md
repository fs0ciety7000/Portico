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
| 1b Terminal TUI | ✅ | 103 + interop (shell réel) | ✅ (voir ci-dessous) | ✅ (tmux 80×24 et 110×28) |
| 1c Terminal GUI | ✅ | 106 + 3 GUI + interop | ✅ (voir ci-dessous) | ✅ (captures démo 3 thèmes + app réelle WebKitGTK) |
| 2 SFTP / tunnels | ⏳ | | | |
| 3 Clés / secrets / snippets | ⏳ | | | |
| 4 Finition | ⏳ | | | |

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
