# Stack technique de Portico : décisions et alternatives

> Ce document est un registre de décisions (ADR léger). Chaque choix indique les contraintes
> qui l'ont motivé, les alternatives évaluées et les conditions qui feraient revenir dessus.
> Versions constatées sur crates.io et npm au 2026-10-19.

## Contraintes qui pilotent les choix

1. **Rust obligatoire pour le cœur**, qui doit être partagé à l'identique par la TUI et la GUI.
2. **Local-first** : aucune dépendance réseau ou cloud, données lisibles.
3. **Design premium = priorité absolue**, surtout en GUI : transitions, micro-interactions, typographie.
4. **SFTP, PTY, forwarding, agent et certificats dès le MVP** : il faut une vraie bibliothèque SSH, pas un wrapper.
5. **Performance et empreinte** : démarrage rapide, faible RAM.
6. **Maintenabilité** par une petite équipe assistée par IA : écosystèmes documentés et stables.

---

## 1. Moteur SSH : `russh` 0.64 ✅

| Option | Pour | Contre | Verdict |
|---|---|---|---|
| **`russh`** (pur Rust, async tokio) | PTY, exec, `direct-tcpip` / `tcpip-forward`, agent client, **auth par certificat OpenSSH**, `known_hosts`, kex post-quantique (`mlkem768x25519`), maintenu activement, pas de code C hors backend crypto | API qui évolue entre mineures (à épingler), quelques algorithmes legacy absents | **Retenu** |
| `ssh2` (libssh2) | Mature, large compatibilité | Bindings C, API **bloquante**, certificats OpenSSH mal supportés, build C sur macOS/Linux | Rejeté |
| `openssh` crate / binaire `ssh` système | Compatibilité parfaite avec `ssh_config` | Pas de contrôle fin des états (statut, latence, prompts), SFTP et forwarding via process, dépend du binaire installé | Rejeté comme moteur principal. **Gardé en réserve** comme repli « system ssh » pour `ProxyCommand` exotique et `Match exec` (post-MVP) |
| `libssh-rs` | Bonne couverture | Binding C, async limité | Rejeté |

**Backend crypto** : `aws-lc-rs` (défaut de russh, audité FIPS). Repli possible sur `ring` via les features si le build pose problème.
**SFTP** : `russh-sftp` 3, qui fonctionne sur un canal du même transport, donc une seule connexion pour le terminal et les fichiers.
**Clés** : on utilise **`russh::keys::ssh_key`** (ré-export, version `=0.7.0-rc.11` épinglée par russh) plutôt qu'une
dépendance `ssh-key` directe, pour garantir un modèle de clé unique sans conflit de versions. Il couvre la génération
ed25519/RSA/ECDSA, le chiffrement OpenSSH (bcrypt-pbkdf) et le parsing des certificats.

## 2. `~/.ssh/config` : parser maison ✅

Les crates existants (`ssh2-config`, `ssh_cfg`) n'implémentent pas fidèlement la sémantique OpenSSH :
*first-obtained-value-wins*, `Include` conditionnel (dans un bloc `Host`), `Match host` évalué sur le `HostName` déjà résolu,
tokens `%h %p %r %u %d %n`, mots-clés multi-valués (`IdentityFile`, `LocalForward`…).
Le parser du module `portico-core::ssh_config` fait environ 1 250 lignes, tests compris (21 tests). Il est **livré**.
Il est lecture seule par conception. `Match exec` n'est volontairement **pas** exécuté : pas d'exécution de commande
au simple affichage d'une liste. Il produit un avertissement visible.

## 3. TUI : `ratatui` 0.30 + `crossterm` 0.29 + `alacritty_terminal` 0.26 ✅

| Option | Verdict |
|---|---|
| **ratatui + crossterm** | Standard de facto, widgets riches, rendu en mode immédiat facile à tester (`TestBackend`), `EventStream` async. **Retenu.** |
| cursive | Modèle retenu en mode, moins flexible pour un rendu custom (terminal embarqué). Rejeté. |
| termwiz | Bas niveau, peu de widgets. Rejeté. |

**Émulateur de terminal embarqué** : `alacritty_terminal` (parser VT, grille, scrollback, modes souris, true color),
rendu dans un widget ratatui custom. L'alternative `vt100` + `tui-term`, plus simple mais moins complète
(scrollback, sélection, reflow), a été rejetée.

## 4. GUI : Tauri 2 + Svelte 5 + xterm.js 6 ✅

C'est la décision la plus structurante. Le critère n° 1 est le *design premium avec transitions fluides*.

| Option | Design / animations | Terminal | Empreinte | Maturité | Verdict |
|---|---|---|---|---|---|
| **Tauri 2 + Svelte 5** | CSS, transitions Svelte natives, typographie web : vélocité maximale | **xterm.js** (moteur de VS Code), addon WebGL | ~30-40 Mo installé, ~80-150 Mo RAM | Stable (2.12), IPC `Channel` binaire ordonné | **Retenu** |
| Electron | Idem | xterm.js | 150-300 Mo RAM, ~100 Mo bundle | Très mature | Rejeté : empreinte, et cœur Rust à exposer en N-API |
| `gpui` (Zed) + gpui-component | Rendu GPU natif superbe | Possible (Zed) | Excellente | API instable, docs rares, peu de recul hors Zed | **Plan B** à réévaluer post-MVP |
| `iced` 0.14 | Correct, animations limitées | `iced_term` (alacritty) | Bonne | Stable mais l'écart avec le premium coûte cher | Rejeté |
| `egui` | Aspect « outil », peu premium | `egui_term` | Excellente | Stable | Rejeté |
| Slint / Freya / Dioxus | Prometteurs | Pas de terminal mûr | Bonne | Jeunes ou en transition | Rejeté |

**Pourquoi Svelte 5 plutôt que React ou Solid** : runes réactives fines (pas de VDOM), transitions et animations
**intégrées** (`fly`, `scale`, `crossfade`), bundle minuscule (~58 Ko de JS pour le shell actuel) et syntaxe concise.
Solid serait équivalent en performance, mais ses primitives d'animation sont moins intégrées.

**Risques identifiés et mitigations**
- *WebKitGTK sur Linux* (perf WebGL variable) : xterm.js retombe sur le renderer DOM. Un **spike perf terminal** est prévu en phase 1c
  (débit `cat` d'un gros fichier, latence de frappe) sur WebKitGTK et WKWebView.
- *Tauri 3 alpha* existe : on reste sur **Tauri 2.x stable**. La migration sera évaluée quand la 3.0 sera stable.
- *Pas de code UI partagé entre TUI et GUI* : c'est assumé. Ce qui est partagé, c'est le **moteur** et les **tokens**,
  ainsi que la logique de recherche (le classement vient de `portico-core` via IPC).

## 5. Secrets ✅

- **`keyring` 4** (feature `v1`) : Keychain macOS et Secret Service Linux (GNOME Keyring, KWallet) via `zbus`, en pur Rust.
  Les entrées sont stockées sous le service `dev.portico` avec des comptes stables (`password:user@host:port`, `passphrase:SHA256:…`).
- **Vault de repli** (phase 3, `vault.rs`) : `~/.local/share/portico/vault.toml` (TOML lisible, champs binaires en base64, `0600`),
  chiffré en **XChaCha20-Poly1305** (`chacha20poly1305` 0.11), avec une clé dérivée par **Argon2id** (`argon2` 0.6 ; m=64 Mio, t=3, p=1,
  sel aléatoire 16 octets) depuis une passphrase maître. Le format est versionné (v2) et l'en-tête authentifié (AAD) ;
  chaque lecture-modification-écriture tient un verrou `flock` sur `vault.toml.lock`, et un compteur de génération chiffré
  refuse un retour arrière du fichier pendant que le vault est déverrouillé.
  Choisi par `secrets::default_store` quand `KeyringStore::is_available()` est faux ; verrouillé au démarrage.
- **En mémoire** : `secrecy::SecretString` (Debug caviardé) et `zeroize` sur les buffers.
- *Évolution prévue* : migrer vers `keyring-core` avec des stores explicites (`apple-native-keyring-store`,
  `zbus-secret-service-keyring-store`, `linux-keyutils-keyring-store`) pour contrôler finement le backend choisi.

## 6. Données et persistance ✅

TOML (`toml` 1.x) dans les répertoires plateforme (`directories` 6) :
- Linux : `~/.config/portico/{hosts,snippets,settings}.toml` et `~/.local/share/portico/`
- macOS : `~/Library/Application Support/dev.Portico.portico/`

L'écriture est **atomique** (fichier temporaire, `fsync`, `rename`) avec des permissions `0600`. SQLite a été rejeté :
le volume est faible, et un format lisible et versionnable sert le principe « zéro lock-in ».

## 7. Transverse

| Besoin | Choix |
|---|---|
| Async | `tokio` 1.x (+ `tokio-util` pour `CancellationToken`) |
| Recherche floue | `nucleo-matcher` 0.3 (Helix) : smart-case, normalisation Unicode |
| Erreurs | `thiserror` 2 (core), `color-eyre` (apps) |
| Logs | `tracing` + `tracing-subscriber` (fichier pour la TUI), plus un `LogRing` par session exposé dans l'UI |
| Frontend | TypeScript 6, Vite 8, `svelte-check`, pnpm |
| Qualité | clippy `pedantic` + `unwrap_used`/`expect_used`/`panic` en *deny*, `cargo-deny` (licences et advisories) en CI |

---

## Decision log

| Date | Décision | Statut |
|---|---|---|
| 2026-10-19 | Workspace : `portico-core`, `portico-theme`, `portico-tui`, `portico-gui` | Accepté |
| 2026-10-19 | russh 0.64 + russh-sftp 3, clés via `russh::keys::ssh_key` | Accepté |
| 2026-10-19 | Parser `ssh_config` maison, lecture seule, `Match exec` non exécuté | Accepté |
| 2026-10-19 | GUI Tauri 2 + Svelte 5 + xterm.js ; gpui gardé en plan B | Accepté |
| 2026-10-19 | Tokens de design en Rust, CSS généré et vérifié par test | Accepté |
| 2026-10-19 | `portico-gui` hors `default-members` (dépend de webkit2gtk sur Linux) | Accepté |
| 2026-10-19 | Plan de contrôle `broadcast` / plan de données `mpsc` borné par terminal | Accepté |
| 2026-10-20 | `known_hosts` implémenté dans Portico (hashed, `@cert-authority`, `@revoked`) au lieu de `russh::keys::known_hosts`, qui ne couvre pas ces cas | Accepté |
| 2026-10-20 | Signer paresseux (`FuturePublicKey`) : passphrase demandée seulement après `PK_OK`, comme OpenSSH | Accepté |
| 2026-10-20 | Clé ignorée après `PK_OK` (passphrase refusée, refus de l'agent) : envoi d'une signature **bien formée mais invalide** pour resynchroniser le protocole (validé contre OpenSSH 9.6 ; Dropbear à valider en CI). Cible : PR upstream russh permettant à un `Signer` d'abandonner | Accepté, à surveiller |
| 2026-10-20 | `ProxyCommand` refusé explicitement tant qu'il n'est pas implémenté (jamais de connexion directe à la place) | Accepté |
| 2026-10-20 | SHA-1 `ssh-rsa` désactivé (clé hôte et auth), comme OpenSSH ≥ 8.8 ; opt-in par hôte post-MVP | Accepté |
| 2026-10-20 | Mot de passe mémorisé lié à `user@nom_known_hosts:port#empreinte_clé_hôte` | Accepté |
| 2026-10-20 | Certificat hôte d'une CA de confiance mais invalide : échec dur, sans repli sur la clé brute (plus strict qu'OpenSSH) | Accepté |
| 2026-10-20 | russh 0.64 ne revérifie pas la clé hôte au re-key : limite connue, ticket upstream à ouvrir | Risque accepté |
| 2026-10-20 | Copies résiduelles de mots de passe en mémoire (russh `String`, webview) : risque accepté pour le MVP ; désactivation des core dumps et du ptrace (`rustix`) en phase 4 | Risque accepté |
| 2026-10-20 | `StrictHostKeyChecking no` traité comme `accept-new` : une clé modifiée est **toujours** refusée (écart volontaire avec OpenSSH) | Accepté |
| 2026-10-20 | Latence = RTT de `keepalive@openssh.com` (`send_ping`), premier échantillon 500 ms après le login | Accepté |
| 2026-10-20 | Terminal : tâches lecteur et écrivain séparées par canal ; russh 0.64 rouvre la fenêtre SSH dès réception, donc la backpressure est TCP et par connexion (pas par canal). Cible : PR upstream pour piloter l'ajustement de fenêtre | Limite upstream acceptée |
| 2026-10-20 | `vte` / `alacritty_terminal` ne bornent pas les séquences OSC ni les caractères combinants : titres assainis et combinants plafonnés côté Portico ; à suivre upstream | Risque surveillé |
| 2026-10-20 | Refus de `pty-req` : échec du terminal (`terminal_refused`), là où `ssh -t` continue sans PTY ; un shell sans PTY est inutilisable dans un onglet | Accepté |
| 2026-10-20 | Écarter le prompt de mot de passe termine la session (`Closed { UserRequested }`) au lieu d'essayer la méthode suivante, comme `Ctrl-C` dans `ssh` | Accepté |
| 2026-10-20 | GUI : octets PTY en brut sur un `ipc::Channel` par terminal, rafales regroupées (64 Kio) et **contrôle de flux par acquittements** (xterm.js acquitte ce qu'il a parsé, 512 Kio max en vol), car `Channel::send` ne bloque jamais | Accepté |
| 2026-10-20 | GUI Linux : modificateur `Ctrl-Shift` dans un terminal (`Ctrl-T/W/K` appartiennent au shell), `⌘` sur macOS ; onglets numérotés en `Alt-n` sur Linux ; lettres reconnues par `e.key` (dispositions non QWERTY) ; `Option` n'est pas `Meta` sur macOS (caractères `|[]{}~` des claviers non US) | Accepté |
| 2026-10-20 | Commandes Tauri qui appellent le moteur en `async` (une commande sync s'exécute sur le thread principal, hors runtime Tokio : `connect` paniquait) | Accepté |
| 2026-10-20 | Rendu xterm.js : WebGL avec repli DOM sur perte de contexte. Spike WebKitGTK (Xvfb, GL logiciel, build debug) : 1 M lignes (6,9 Mo) en 1,9 s de bout en bout, mémoire stable sous flot continu, `Ctrl-C` immédiat. WKWebView (macOS) à mesurer sur matériel réel | Accepté, à mesurer sur macOS |
| 2026-10-20 | Polices embarquées via `@fontsource` (Inter variable, JetBrains Mono 400/600, Geist 500/600, sous-ensemble latin), compatibles CSP `font-src 'self'` ; xterm créé après chargement de la police mono | Accepté |
| 2026-10-20 | `Engine::disconnect` annule le jeton de session (fonctionne aussi pendant la connexion) ; les prompts en attente sont retirés (`PromptResolved`) quel que soit le chemin d'abandon | Accepté |
| 2026-10-20 | GUI : un rechargement de page ferme terminaux et sessions (`disconnect_all`), car la page a perdu ses Channels et dialogues ; à la fermeture, attente bornée (500 ms) des fermetures propres (`shutdown_and_wait`) | Accepté |
| 2026-10-20 | Contrôle de flux GUI : acquittements monotones et plafonnés aux octets réellement envoyés ; entrée sérialisée (un seul envoi IPC en vol), morceaux de 64 Kio sans couper de paire UTF-16 | Accepté |
| 2026-10-20 | Dialogues : armement après 400 ms **sans frappe** (pas seulement après ouverture), `y` accepté 150 ms après l'armement ; textes keyboard-interactive du serveur assainis et étiquetés « Message from … » | Accepté |
| 2026-10-20 | Liens OSC 8 inertes dans xterm.js jusqu'à un gestionnaire explicite (confirmation Portico, http(s) seulement, post-MVP) | Accepté |
| 2026-10-20 | Capabilities Tauri réduites à `event:listen/unlisten` et `window:start-dragging/internal-toggle-maximize` ; backend de démo uniquement en dev ou `--mode demo` | Accepté |
| 2026-10-20 | Thèmes : distance minimale ΔE ≥ 20 (CIE76) entre l'accent et chaque statut, testée. Corrigés : Obsidian `warning` #f2c14e, Basalt accent étain #b4bcc6 et `connecting` cyan #6fc3d3, Travertine accent #c99a74 | Accepté |
| 2026-10-20 | Tokens ajoutés : `--shadow-raised/overlay`, `--text-display(-sm)`, `--motion-loop` (seule animation > 260 ms, indicateurs en boucle) ; transitions Svelte via `dur()` (0 si `prefers-reduced-motion`) | Accepté |
| 2026-10-20 | Police display (wordmark, titres d'hôtes) : **Geist** à la place de Cormorant Garamond, choix du propriétaire sur planche comparative (10 options) ; grotesque nette, distincte d'Inter par ses proportions et l'espacement | Accepté |
| 2026-10-21 | SFTP : un client par session (rouvert seulement après une erreur de transport, jamais après un refus du serveur) ; transferts en chunks, progression bornée, annulation par jeton enfant de la session | Accepté |
| 2026-10-21 | Fichier partiel `<dest>.<id>.part` créé en exclusif (`O_EXCL`) avec le mode de la source. Finalisation non annulable : en local `hard_link` (pas de course vérifier-puis-écrire), à distance la destination est mise de côté puis restaurée en cas d'échec. `posix-rename@openssh.com` reporté | Accepté |
| 2026-10-21 | Noms fournis par le serveur : affichés via `display_safe` (contrôles et bidi retirés, longueur bornée), destinations locales construites **uniquement** par `local_destination` (séparateurs, `.`/`..`, contrôles refusés) ; noms > 4 096 octets écartés du listing ; listing limité dans le temps (60 s), pas en nombre d'entrées | Accepté |
| 2026-10-21 | russh 0.64 sert tous les canaux d'une session dans **une seule boucle** qui attend la file de chaque canal : un pair local qui ne lit plus (`-L`/`-R`) gèlerait la session entière. `pipe` intercale une file bornée et coupe le pair au-delà de `min(2 s, budget keepalive / 2)` (plancher 100 ms), vérifié contre OpenSSH (interop, scénario 11). Cible : PR upstream (contrôle de flux par canal) | Limite upstream contournée |
| 2026-10-21 | `-L` : écoute sur toutes les adresses du nom de bind, même port (port 0 : le premier choisi) ; `*` donne `0.0.0.0` (IPv4 seulement, comme OpenSSH sans `AddressFamily`) ; toute écoute hors loopback exige une confirmation explicite (`allow_exposed`, erreur `tunnel_exposed` sinon) | Accepté |
| 2026-10-21 | Canaux ouverts par le serveur : seuls les `forwarded-tcpip` correspondant à un `-R` demandé sont acceptés ; agent, X11, streamlocal, `session` et `direct-tcpip` entrants sont refusés | Accepté |
| 2026-10-21 | Arrêter un tunnel ferme aussi ses connexions en cours (OpenSSH les laisse finir) : l'utilisateur qui arrête veut que le port ne serve plus | Accepté |
| 2026-10-21 | Forwards de `ssh_config` (`LocalForward`, `RemoteForward`) proposés dans le panneau Tunnels, jamais démarrés d'office ; syntaxe utilisateur `-L …` / `-R …` analysée par `ForwardSpec::parse_cli`, partagée par les deux UIs | Accepté |
| 2026-10-21 | GUI : pas de panneau de fichiers locaux. Le webview ne manipule jamais un chemin local : uploads = jetons émis par Rust pour les fichiers choisis dans le sélecteur natif ou déposés sur la fenêtre (effacés au rechargement), downloads dans un dossier tenu par Rust (Téléchargements par défaut, modifiable par sélecteur natif). `tauri-plugin-dialog` utilisé depuis Rust seulement (aucune permission `dialog:`), épinglé `=2.8.1` (version de plus de deux semaines) | Accepté |
| 2026-10-21 | Raccourcis : TUI `f` (fichiers) et `p` (tunnels) sur la liste d'hôtes, `Ctrl-Space f/p` partout ; GUI `⌘⇧F` / `⌘⇧P` (macOS, `⌘F`/`⌘P` restent libres), `Ctrl-Shift-F/P` (Linux). Le favori (phase 4) passe sur `*` | Accepté |
| 2026-10-21 | Captures GUI sous Xvfb avec `WEBKIT_DISABLE_COMPOSITING_MODE=1` : le canvas d'un terminal jamais focalisé n'y est pas repeint ; avec composition (mode normal d'un bureau) tout s'affiche. Artefact de l'environnement de capture, pas de contournement dans le code | Noté |
| 2026-10-21 | GUI, jetons d'upload : liés à la session et au chemin distant du premier envoi, consommés par la réponse finale (`overwrite`, `skip`, `rename`), expirés après 10 min. Dossier de téléchargement par défaut jamais égal au home (créé en `0700` si absent) ; un download ne remplace jamais un fichier caché (`hidden_overwrite`) | Accepté |
| 2026-10-21 | Tauri annonce au webview les chemins des fichiers déposés (`tauri://drag-drop`) et les ajoute au scope d'assets : fuite de noms seulement, tant que **ni la feature `protocol-asset` ni `tauri-plugin-fs` ne sont activées** (test `tauri_never_gets_the_asset_protocol_or_the_fs_plugin`) | Accepté |
| 2026-10-21 | Confirmation d'un tunnel exposé : réarmée à son apparition et après chaque frappe ou clic (400 ms), focus sur Annuler, liste des tunnels inerte pendant la question | Accepté |
| 2026-10-21 | Génération de clés : `ssh_key` de russh (une seule pile de clés), aléa du système (`SysRng`, getrandom ; voir la ligne des revues), format OpenSSH (bcrypt-pbkdf + aes256-ctr comme `ssh-keygen`). Privée créée en exclusif `0600` (jamais d'écrasement, lien symbolique compris), `.pub` en `0644` ; une paire incomplète est supprimée. RSA 3072–8192 bits | Accepté |
| 2026-10-21 | Copy-id : canal `exec` sur la session déjà authentifiée, script POSIX via `exec sh -c '…'` (sans `'` ni `\\`, compatible fish/csh), clé transmise sur **stdin** et reconstruite depuis la clé parsée (ni options, ni lignes en plus, pas de certificat). Idempotent sur le blob, `umask 077`, ne réécrit jamais les lignes existantes. Vérification par reconnexion `publickey` seul avec cette clé seule (`ConnectRequest::only_identity`, session marquée `probe`) | Accepté |
| 2026-10-21 | Agent : liste, ajout (passphrase en mémoire seulement, durée de vie optionnelle) et retrait via le client russh. Limites russh : commentaire vide à l'ajout, retrait d'un certificat impossible (refusé explicitement plutôt que retirer la mauvaise identité) | Accepté |
| 2026-10-21 | Revues phase 3 (keys/copy-id/agent) : script copy-id sur **une seule ligne** sans `'`, `!` ni `\\` (testé sous sh, dash, bash, fish, tcsh, csh), résultat lu sur une ligne marqueur (`portico-copy-id:…`, insensible aux rc bavards), données envoyées après l'acceptation de `exec`, canal toujours fermé, pas de `chmod` sur l'existant, `restorecon` si présent. Vérification : `publickey` seul, **sans certificat** (ni fichier voisin, ni agent) et sans `none`, même compte que la session de copy-id, session sonde fermée si l'appel est abandonné, erreurs d'origine conservées (`host_key_changed`…). Aléa : `SysRng` (getrandom) sondé puis `UnwrapErr`. `~/.ssh` accessible en écriture à d'autres comptes : génération refusée | Accepté |
| 2026-10-21 | Vault de repli `vault.toml` **v2** (plutôt qu'un binaire : lisible, diffable, versionné) : Argon2id → 64 octets (clé de chiffrement + clé de contrôle), paramètres et sel dans le fichier, bornés **avant** toute dérivation (plancher = paramètres par défaut 64 Mio/t=3/p=1, un fichier ne peut pas affaiblir la KDF ; plafond 256 Mio/t=8/p=4, pas de gel) ; contenu `{"generation": n, "entries": {compte: base64(secret)}}` (base64 : aucun échappement JSON, donc aucune copie en clair hors `Zeroizing`) rembourré à 256 octets, XChaCha20-Poly1305 avec nonce aléatoire de 24 octets à chaque écriture ; AAD = rendu canonique de l'en-tête. Un tag de contrôle (message vide, clé de contrôle) distingue `vault_wrong_passphrase` de `vault_corrupt`. Création sans écrasement (`hard_link`), écritures atomiques qui relisent le fichier. **Verrou inter-processus** : `std::fs::File::lock` (stable 1.89) sur `vault.toml.lock` (`0600`) autour de chaque lecture-modification-écriture (création, écriture, changement de passphrase) : un re-clé ne peut plus être annulé par un écrivain concurrent ; re-clé détecté : `vault_changed`. **Anti-rollback** : génération incrémentée à chaque écriture et gardée en mémoire ; une génération relue plus ancienne est refusée (`vault_rollback`, verrouillage) ; protection limitée à la durée de vie du handle déverrouillé. v1 (jamais publiée) refusée par `vault_version` (« recreate the vault »), sans migration. Verrouillé : la connexion continue, sans mémorisation (journal de session, avec le code d'erreur) | Accepté |
| 2026-10-21 | Snippets : rendu sûr `render_checked` (substitution en une passe, toute valeur requise, valeurs sur une ligne sans contrôle, corps sans ESC, C0 hors `\t`/`\n`, DEL, C1 ni bidi, ni séparateurs U+2028/U+2029, ni caractères de format invisibles hors ZWJ, vérifié à l'enregistrement, au chargement **et** au rendu) ; envoi par `snippet_input` → `Result` (sauts de ligne en `\r`, bracketed paste si l'application distante l'a activé, `\r` final si `run` ; un corps multi-ligne collé sans `run` ni bracketed paste est refusé : `snippet_needs_paste_mode`), jamais d'exécution locale. `snippets.toml` lu avec un plafond de 1 Mio ; entrées invalides masquées (conservées dans le fichier, `warn` sans contenu). Ids slug `[a-z0-9-]` dérivés du nom (accents repliés), uniques (doublons et ids malformés redérivés au chargement). `store::write_temp` : nom temporaire aléatoire, `create_new`, dossiers créés en `0700` | Accepté |
| 2026-10-21 | `hosts.toml` (phase 4) : édition par `Engine::set_favorite` / `set_tags` / `set_group` / `set_notes` = lecture-modification-écriture du **document** TOML (`toml_edit` 0.25, déjà dans l'arbre via keyring) sous verrou inter-processus `hosts.toml.lock` (`store::lock_exclusive`), écriture atomique `0600`. Seule la clé modifiée de l'alias modifié change : autres alias, **clés inconnues à tous les niveaux, commentaires et mise en page** conservés (préféré à `#[serde(flatten)] extra: toml::Table`, qui perd les commentaires et ferait fuiter les clés inconnues dans les événements). Valeur par défaut = clé retirée, alias vide retiré. Fichier rejeté par le lecteur typé (ou valeur non-table) : jamais réécrit. Validation : tags `[A-Za-z0-9_.-]{1,32}`, `#` initial retiré, doublons sans casse, ≤ 16 ; groupe ≤ 64 caractères, notes ≤ 2000 (`\n` seul contrôle, `\r\n` normalisé), bidi et invisibles refusés ; lecture plafonnée à 1 Mio. Événement `EngineEvent::HostMeta { alias, meta }`. `last_used` déprécié : lu (leniment), jamais sérialisé ni écrit. Recherche : filtre `@groupe` (préfixe, sans casse) | Accepté |
| 2026-10-21 | Sessions récentes : `state/recent.toml` dans le **data dir** (état, pas configuration ; dossier `0700`, fichier `0600`), liste `[[recent]] { alias, last (RFC 3339), count }` triée par date, une entrée par alias, bornée à 50, réécrite entière sous verrou `recent.toml.lock`. Enregistrée quand une session non-sonde atteint `Connected`, en tâche de fond (`spawn_blocking`) : un échec n'est que journalisé (journal de session + `tracing`), puis `EngineEvent::RecentChanged`. Fichier illisible : erreur à la lecture, `clear_recent` le répare | Accepté |
| 2026-10-21 | Logs de debug : l'entrée d'une session reste dans la table après sa fin (marquée `ended` + `SessionOutcome`), les 20 dernières sessions terminées gardent leurs logs (sondes copy-id comprises, marquées `probe`) ; au-delà, la plus ancienne est oubliée (`unknown_session`). `LogRecord::to_line()` : `AAAA-MM-JJTHH:MM:SS.mmmZ NIVEAU cible: message`, UTC, contrôles, bidi et invisibles échappés (une ligne par enregistrement) | Accepté |
| 2026-10-21 | Config effective (`ssh -G`) : `host`, `hostname`, `user`, `port` résolus en tête, puis options configurées par ordre alphabétique avec leur origine `fichier:ligne` (déjà connue du résolveur : une ligne par valeur des options multi-valuées), puis les **défauts appliqués par Portico** (`connecttimeout`, `serveraliveinterval`, `serveralivecountmax`, `identityfile`, `userknownhostsfile`, `globalknownhostsfile`, `identitiesonly`, `stricthostkeychecking`) marqués `default`, et la route `ProxyJump` calculée par le même `route::plan` que les sessions (erreur de route rendue en `ErrorSummary`) | Accepté |
| 2026-10-21 | `settings.toml` (config dir) : `theme`, `keymap.tui`, `keymap.gui` (action → touche, syntaxe propre à chaque UI, non validée par le core) ; bornes ≤ 200 entrées par keymap, chaînes ≤ 64 caractères sans contrôle ni bidi, fichier ≤ 256 Kio. Écriture comme `hosts.toml` (document `toml_edit`, verrou, clés inconnues et commentaires conservés). Fichier invalide : erreur `settings_invalid`, les UIs reprennent leurs défauts | Accepté |
| 2026-10-21 | Durcissement du processus (`portico_core::hardening::harden_process`, appelé en tête de `main` des deux apps, jamais fatal, rapport journalisé) : `RLIMIT_CORE = 0` (souple et dur) et, sur Linux, `PR_SET_DUMPABLE = 0` (plus de `ptrace` ni de `/proc/<pid>/mem` par un autre processus du même utilisateur). **Portée exacte** : `PR_SET_DUMPABLE` protège le processus Rust seulement ; le noyau le remet à 1 à l'`execve`, donc les processus `WebKitWebProcess`/`WebKitNetworkProcess` que WebKitGTK lance par `exec` redeviennent traçables par le même utilisateur (le web process voit brièvement ce qui est saisi dans un prompt ; clés, clé du vault et transport SSH restent dans le processus Rust ; `ptrace_scope` de Yama ≥ 1 comble l'écart sur la plupart des distributions). Les core dumps restent bloqués pour tous les descendants, car la limite dure `RLIMIT_CORE = 0` est héritée et ne peut pas être relevée sans privilège. API sûres de `rustix` 1.1 (déjà dans l'arbre), `#![forbid(unsafe_code)]` maintenu. macOS : aucune API sûre pour `PT_DENY_ATTACH` dans rustix ni nix ; on s'appuie sur le hardened runtime de l'app signée (sans `get-task-allow`), la TUI n'a que `RLIMIT_CORE` | Accepté, limite macOS |
| 2026-10-21 | Packaging (phase 4) : workflow GitHub Actions maison `release.yml` plutôt que cargo-dist (cargo-dist génère et possède son propre workflow, ne sait pas assembler les bundles Tauri dans la même Release ; un workflow lisible suffit pour 3 familles d'artefacts). Tag `v*` → Release **brouillon** ; `workflow_dispatch` = essai à blanc. Versions Cargo / `tauri.conf.json` / `package.json` / tag vérifiées égales. Sommes `SHA256SUMS` + `.sha256` par fichier. Procédure : `docs/RELEASE.md` | Accepté |
| 2026-10-21 | GUI : `.deb` + AppImage (Ubuntu 22.04, glibc 2.35, x86_64 et aarch64), `.dmg` **universel** macOS ≥ 13.0 (cible `es2022` de Vite = Safari ≥ 15.4 ; macOS 12 hors support sécurité Apple). Runtime renforcé, **sans App Sandbox** (lecture de `~/.ssh`, socket `ssh-agent`), un seul droit `network.client`, aucune exception (JIT, DYLD, validation des bibliothèques). `NSLocalNetworkUsageDescription` pour macOS 15. Signature Developer ID et notarisation par le Tauri bundler via les secrets `APPLE_*` ; sans secrets : signature ad hoc et dmg suffixé `-unsigned` (`-not-notarized` si seule la signature est disponible). Icône macOS sur la grille Big Sur (`packaging/icons/portico-macos.svg`, marge de 100/1024), icônes Linux depuis `design/portico-mark.svg` | Accepté |
| 2026-10-21 | TUI : tarballs `portico-<v>-<target>.tar.gz` pour `x86_64`/`aarch64` Linux **gnu** (recommandé : résolution NSS, mDNS `.local`, sssd) **et musl** (statique, portable ; build vérifié localement, 13 Mo), et macOS `x86_64`/`aarch64` (déploiement ≥ 11.0). Profil `release` inchangé (LTO thin, 1 CGU, strip). Pas d'updater Tauri tant qu'aucune ADR ne fixe clé de signature et canal | Accepté |
| 2026-10-21 | Revue sécurité 4a, fichiers Portico : `hosts.toml` est validé **à la lecture** avec les règles de l'édition (`normalize_tags/group/notes`, `check_alias`) ; une valeur invalide est ignorée (le reste de l'alias est gardé, un alias invalide retire l'entrée) avec un avertissement d'inventaire (fichier, ligne, alias, champ ; jamais la valeur). Écritures de `hosts.toml` et `settings.toml` par `store::write_atomic_capped` : refus avant écriture d'un contenu plus gros que le plafond de lecture (`file_too_large`) et d'une destination qui n'est pas un fichier régulier (`not_regular_file` : le `rename` remplacerait un lien symbolique). `read_capped` ne lit qu'un fichier régulier (lien suivi), refuse FIFO, répertoire et périphérique avant ouverture, et ouvre en `O_NONBLOCK` (feature `fs` de `rustix`, déjà dans l'arbre) | Accepté |
| 2026-10-21 | Vue « config effective » : `SetEnv` masqué dans le core (`NOM=•••`, la valeur ne sort jamais) ; `ProxyCommand`, `LocalCommand`, `RemoteCommand`, `KnownHostsCommand` gardés tels quels mais marqués `sensitive: true` (masqués par défaut dans les UIs, jamais journalisés) ; `PermitLocalCommand` (oui/non) n'est pas sensible | Accepté |
| 2026-10-21 | Release, revue sécurité 4a : signature macOS isolée dans `gui-macos-sign` (environnement GitHub `release` restreint aux tags `v*` avec reviewers requis, seuls détenteurs des 6 secrets Apple, outils Apple uniquement : `codesign`, `hdiutil`, `notarytool`, `stapler`) ; le build macOS ne voit aucun secret (signature *ad hoc*). Aucun cache restauré dans `release.yml`. Actions épinglées par SHA (versions publiées depuis plus de deux semaines), `persist-credentials: false`, pnpm exact. Outillage AppImage épinglé (releases datées, plus de `continuous`) et vérifié par `sha256sum -c` dans le cache d'outils de Tauri, runtime de type 2 fourni par `LDAI_RUNTIME_FILE` ; le build échoue si son journal montre un téléchargement. Alternative écartée : signer dans le job de build via les variables du Tauri bundler (exposait les secrets à cargo, pnpm et toutes les dépendances npm) | Accepté |
| 2026-10-21 | Audit final MVP : `cargo deny` remis au vert. Apps marquées `publish = false` (dépendances par chemin). Avis triés par ID avec raison dans `deny.toml` : RUSTSEC-2023-0071 (`rsa`, Marvin : aucun déchiffrement RSA dans Portico, pas de correctif publié), RUSTSEC-2026-0370/0371/0372 (`zvariant`, `zbus_names`, `zvariant_derive` fusionnés dans zbus 6, transitifs via keyring → zbus 5), RUSTSEC-2024-0370 (`proc-macro-error`, build seulement, via glib-macros 0.18). À revoir au passage de keyring à zbus 6 et de Tauri hors gtk-rs 0.18. Log TUI déplacé de `/tmp` vers le dossier de données (`0600`, lien refusé). `SECURITY.md` liste les risques résiduels | Accepté |
| 2026-10-21 | **`-D` (SOCKS5)**, module `socks` sous `forward` : serveur RFC 1928 **minimal et strict**, sans dépendance. Méthode `0x00` (sans authentification) seulement, sinon `0xFF` et fermeture ; commande `CONNECT` seulement (`BIND`, `UDP ASSOCIATE` → `0x07`) ; ATYP IPv4, IPv6 et domaine (autre → `0x08`) ; nom non vide, UTF-8 sans contrôle ni espace, port ≠ 0 (sinon `0x01`). **Résolution distante** : un domaine n'est jamais résolu localement, il part tel quel dans `direct-tcpip` et sshd le résout (comportement de `curl --socks5-hostname`, pas de fuite DNS locale). **Réponse finale après la confirmation du canal** (OpenSSH répond succès avant) : `0x00` ouvert, `ConnectFailed` → `0x05` (sshd ne distingue pas port fermé et hôte inconnu ; russh ne garde pas son texte), `AdministrativelyProhibited` → `0x02`, délai d'ouverture (15 s) → `0x04`, autre → `0x01` ; adresse liée `0.0.0.0:0`, comme OpenSSH. **SOCKS4/4a refusés** (réponse SOCKS4 `91`, erreur nette côté client) : pas d'IPv6, champ user-id non borné par le protocole, tous les clients courants parlent SOCKS5. Lectures à taille fixe (≤ 257 octets de salut, ≤ 262 de requête), poignée de main bornée à 10 s, seuls les octets du protocole sont consommés (données envoyées d'avance gardées) ; requête lue en entier avant un refus, puis FIN et vidange ≤ 500 ms, sans quoi la fermeture sur données non lues (RST) perdrait la réponse. **Écoute** : exactement la mécanique `-L` (toutes les adresses du nom, `localhost` par défaut, `*` ou vide → `0.0.0.0`, hors loopback : `allow_exposed` sinon `tunnel_exposed`, 256 connexions, pipe borné qui coupe un pair lent, arrêt qui coupe tout, poignées de main comprises). Un proxy SOCKS exposé est un relais ouvert vers tout ce que le serveur atteint : d'où la confirmation armée. `DynamicForward` proposé dans le panneau Tunnels, jamais démarré d'office ; affichage `SOCKS · localhost:1080` | Accepté |
