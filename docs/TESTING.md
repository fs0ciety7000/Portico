# Tests de Portico

| Niveau | Où | Commande | En CI |
|---|---|---|---|
| Unitaires Rust (core, thème, TUI, commandes GUI) | `#[cfg(test)]` à côté du code | `cargo test` | oui |
| Interopérabilité OpenSSH (`sshd`, `ssh-agent`, `ssh-keygen`) | `crates/portico-core/tests/openssh_interop.rs` | `cargo test -p portico-core --test openssh_interop -- --ignored` | oui |
| Logique TypeScript de la GUI (raccourcis, liste d'hôtes) | `apps/portico-gui/scripts/test-*.ts` | `pnpm check` | oui |
| **E2E de l'app de bureau** (Tauri sous WebKitGTK, vrai `sshd`) | `apps/portico-gui/e2e/` | `pnpm e2e` | non (voir plus bas) |

Le mode démo (`pnpm dev`, Playwright) sert au design ; il ne remplace pas l'E2E : seul ce dernier passe par
l'IPC Tauri, le moteur réel, WebKitGTK et un serveur SSH.

## E2E de l'app de bureau

`apps/portico-gui/e2e/run.sh` (alias `pnpm e2e`, Linux uniquement) :

1. construit l'app en debug avec le frontend embarqué (`pnpm tauri build --debug --no-bundle`, binaire dans
   `$CARGO_TARGET_DIR/debug/portico-gui`) ; `--no-build` réutilise le binaire existant ;
2. crée un environnement jetable (`e2e/env.sh`) : `HOME` et dossiers XDG à lui (jamais le vrai `~/.ssh`),
   un `sshd` OpenSSH sur `127.0.0.1` (port libre, clé d'amorçage, `SetEnv HOME` vers un HOME distant jetable,
   SFTP démarrant dans ce HOME), un `ssh-agent` (`SSH_AUTH_SOCK`), un petit serveur HTTP pour le tunnel et un
   `~/.ssh/config` à trois hôtes : `alpha` (avec une valeur `SetEnv` qui doit rester masquée), `jumped`
   (`ProxyJump alpha`) et `gamma` (n'accepte que la clé générée pendant le test). Pas de bus de session D-Bus :
   pas de keyring système, donc le **vault chiffré** est le store, comme sur un serveur ou un WM minimal ;
3. lance `Xvfb`, puis `tauri-driver` (qui pilote `WebKitWebDriver`) ;
4. exécute `e2e/gui.e2e.ts` (runner de test de Node, client WebDriver minimal sans dépendance dans
   `e2e/webdriver.ts`).

Le scénario, dans l'ordre : démarrage (hôtes, pas de vault) ; connexion et clé hôte acceptée (`known_hosts`) ;
commande dans le terminal, dont la sortie revient par le PTY (le shell pose un titre OSC que la barre d'état
affiche) ; statut et latence ; création du vault ; génération d'une clé ed25519 avec passphrase ; ajout à
l'agent pour une heure puis retrait (`ssh-add -l`) ; copy-id vers `alpha`, vérification avec la clé (prompt de
passphrase, mémorisée dans le vault) ; connexion à `gamma` avec cette seule clé, sans prompt ; téléchargement
SFTP ; snippet avec placeholder, puis refus du collage multi-ligne sous `sh` (sans bracketed paste) et
exécution à la place ; favori, tags, groupe et notes (`hosts.toml`), la sélection suivant l'hôte déplacé ;
ProxyJump ; panneau de logs et configuration effective (valeur sensible masquée, absente du DOM) ; tunnel
`-L` (affiché avec sa spec, trafic HTTP, arrêt) ; thème mémorisé (`settings.toml`) ; redémarrage : déverrouillage
du vault au démarrage et connexion par clé sans prompt.

Une capture est prise à chaque étape clé (et à chaque échec d'attente) dans `<racine>/shots/`. La racine
jetable est supprimée à la fin, sauf avec `E2E_KEEP=1` ou si elle est donnée par `E2E_ROOT=<dossier>`.

### Prérequis (Debian/Ubuntu)

```bash
sudo apt install openssh-server openssh-client xvfb webkit2gtk-driver libwebkit2gtk-4.1-dev
cargo install tauri-driver --version 2.1.0 --locked
```

Node 22.18+ (types TypeScript retirés nativement) et pnpm. `sshd` tourne sous l'utilisateur courant (le test
se connecte en tant que lui) ; sous root il lui faut `/run/sshd`, que le script crée.

```bash
cd apps/portico-gui
pnpm e2e                                  # build debug + test (~1 min hors build)
E2E_KEEP=1 e2e/run.sh --no-build          # garder captures et logs (sshd.log, driver.log)
e2e/run.sh --serve --no-build             # environnement prêt, WebDriver sur un port aléatoire, pour piloter à la main
```

> **Attention, `--serve`** : `tauri-driver` lance tout programme qu'une requête WebDriver lui désigne, et une
> page web peut l'atteindre par une requête cross-origin « simple » (sans preflight). Les ports sont donc
> aléatoires par défaut (`E2E_DRIVER_PORT` et `E2E_NATIVE_PORT` pour les fixer ; `4444` refusé en `--serve`
> sauf `E2E_I_KNOW=1`). Pendant un run ou un `--serve`, ne naviguez pas, et jamais sur une machine partagée.

### Limites

- **Pas en CI** : il faut WebKitGTK, son driver WebDriver, `tauri-driver`, Xvfb et un `sshd` ; à lancer avant une
  release et après toute modification notable de la GUI.
- **Linux seulement** : `tauri-driver` ne pilote pas WKWebView (macOS).
- Le sélecteur de fichiers natif (upload SFTP, dossier de téléchargement) est un dialogue GTK hors WebDriver :
  l'upload a été vérifié à la main (`xdotool` : `Ctrl-L`, chemin, Entrée), pas dans le test.
- Le contenu du terminal est rendu par xterm.js (WebGL ou canvas), illisible par WebDriver : le test observe ses
  effets (titre OSC, fichiers distants) et les captures montrent le rendu.
- Le presse-papiers (copie des logs) se vérifie à la main sous Xvfb (`xsel -b -o`).
