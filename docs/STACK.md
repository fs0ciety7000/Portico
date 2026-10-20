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
- **Vault de repli** (phase 3) : `~/.local/share/portico/vault.bin`, chiffré en **XChaCha20-Poly1305** (`chacha20poly1305` 0.11),
  avec une clé dérivée par **Argon2id** (`argon2` 0.6 ; m=64 Mio, t=3, p=1, sel aléatoire 16 octets) depuis une passphrase maître.
  Le format est versionné et l'en-tête authentifié (AAD).
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
