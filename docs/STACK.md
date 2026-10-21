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
  sel aléatoire 16 octets) depuis une passphrase maître. Le format est versionné et l'en-tête authentifié (AAD).
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
| 2026-10-21 | Vault de repli `vault.toml` v1 (plutôt qu'un binaire : lisible, diffable, versionné) : Argon2id → 64 octets (clé de chiffrement + clé de contrôle), paramètres et sel dans le fichier, bornés **avant** toute dérivation (plancher OWASP 19 Mio/t=2, plafond 1 Gio/t=10/p=8) ; toute la table compte → secret en un bloc JSON rembourré à 256 octets, XChaCha20-Poly1305 avec nonce aléatoire de 24 octets à chaque écriture ; AAD = rendu canonique de l'en-tête. Un tag de contrôle (message vide, clé de contrôle) distingue `vault_wrong_passphrase` de `vault_corrupt`. Création sans écrasement (`hard_link`), écritures atomiques qui relisent le fichier (pas de verrou inter-processus ; re-clé détecté : `vault_changed`). Verrouillé : la connexion continue, sans mémorisation (journal de session) | Accepté |
| 2026-10-21 | Snippets : rendu sûr `render_checked` (substitution en une passe, toute valeur requise, valeurs sur une ligne sans contrôle, corps sans ESC, C0 hors `\t`/`\n`, DEL, C1 ni bidi, vérifié à l'enregistrement **et** au rendu) ; envoi par `snippet_input` (sauts de ligne en `\r`, bracketed paste si l'application distante l'a activé, `\r` final si `run`), jamais d'exécution locale. Ids slug `[a-z0-9-]` dérivés du nom (accents repliés), uniques | Accepté |
