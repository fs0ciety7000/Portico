# PERF : mesures de performance (phase 4)

> Objectifs de `CLAUDE.md` §2 : TUI utilisable en moins de **100 ms** à froid, première fenêtre GUI en moins de
> **500 ms**, **aucun redraw** quand rien ne change, chargement des hôtes **jamais bloquant**. `docs/PLAN.md`
> phase 4 demande aussi la RAM au repos et avec 5 sessions. Ce document donne des mesures reproductibles
> (`bench/`) sans modification du code produit. Mesures du 2026-10-21 sur `c3f0960`, builds release.

## Synthèse

| Objectif | Mesure (médiane / p95) | Verdict |
|---|---|---|
| TUI utilisable < 100 ms à froid (50 hôtes) | cache vidé : liste complète en **25 / 31 ms** ; cache chaud : **11 / 14 ms** | ✅ marge ×3 à froid |
| GUI : première fenêtre < 500 ms | fenêtre mappée (fond du thème) : **73 / 93 ms** à chaud, **155 / 197 ms** cache vidé | ✅ pour la fenêtre |
| GUI : premier rendu de l'interface (Xvfb, sans GPU) | premier rendu web : **1 148 / 1 192 ms** à chaud, **1 464 / 2 276 ms** cache vidé ; liste d'hôtes +70 ms | ❌ dépassement ×2,3 (environnement défavorable, voir limites) |
| Pas de redraw au repos : TUI | 30 s sur la liste, puis sur un onglet terminal (1 et 5 sessions) : **0 octet** écrit sur le pty, 0 à 1 tick CPU (≤ 0,03 %) | ✅ |
| Pas de redraw au repos : GUI | liste d'hôtes : **0,13 %** CPU (tous processus). Onglet terminal focalisé : **12 %** CPU, presque tout dans `WebKitWebProcess` | ✅ liste ; ❌ terminal (curseur clignotant probable) |
| Chargement des hôtes non bloquant | 1er rendu à **8 ms** quel que soit le volume ; la touche `t` est traitée en **~23 ms** pendant le chargement de 2 000 hôtes | ✅ non bloquant |
| Durée du chargement des hôtes | 200 : 30 ms ; 500 : 103 ms ; 1 000 : 362 ms ; **2 000 : 1,54 s** ; 10 000 : **47 s** (croissance quadratique) | ⚠️ pas un objectif chiffré, mais à corriger |
| RAM TUI | liste (52 hôtes) : **7,7 Mio PSS** (9,7 Mio RSS) ; 5 sessions : **12,4 Mio PSS** (14,6 Mio RSS) | pas d'objectif chiffré |
| RAM GUI (somme des 3 processus) | liste : **388 Mio PSS** (558 Mio RSS) ; 5 sessions : **547 Mio PSS** (719 Mio RSS) | pas d'objectif chiffré ; WebKit ≈ 75 % du PSS |

## Environnement

| Élément | Valeur |
|---|---|
| Machine | VM cloud (Firecracker, noyau `6.18.44-fc-v139`, conteneur Docker), 4 vCPU Intel Xeon @ 2,80 GHz, 15 Gio de RAM, sans swap, sans GPU |
| OS | Ubuntu 24.04.5 LTS |
| Toolchain | rustc 1.97.0, profil `release` du workspace (`lto = "thin"`, `codegen-units = 1`, `strip`) |
| Crates | russh 0.64.1, ratatui 0.30.2, crossterm 0.29.0, alacritty_terminal 0.26.0, tokio 1.54.0, tauri 2.12.4, wry 0.57.0 |
| WebView | WebKitGTK 4.1 2.52.6 (`libwebkit2gtk-4.1-0`), sous Xvfb 21.1.12, rendu logiciel (pas de GPU) |
| Binaires | `portico` 13,6 Mo ; `portico-gui` 18,8 Mo (bundle web embarqué : JS 694 Ko en un seul chunk, CSS 81 Ko, polices) |
| SSH | OpenSSH 9.6p1 (sshd jetable sur 127.0.0.1) |
| Outils de mesure | Python 3.13, `pyte` 0.8.2 (émulateur VT), `python-xlib` 0.33, strace 6.8, xdotool, ImageMagick `import` |

## Méthode

Tout passe par `bench/perf.py` (harnais) et `bench/fixtures.py` (config jetable) ; `bench/run-all.sh` rejoue
l'ensemble et écrit un JSON par mesure. Les résultats bruts de ce document sont dans `bench/results/2026-10-21/`.

```bash
cargo build --release -p portico-tui
(cd apps/portico-gui && pnpm build)
cargo build --release -p portico-gui --features tauri/custom-protocol   # équivaut à `pnpm tauri build --no-bundle`
pip install pyte python-xlib && apt install xvfb xdotool x11-utils imagemagick strace
sudo bench/run-all.sh bench/out          # root : drop_caches et sshd
```

**Isolation.** Chaque mesure crée une racine temporaire : `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`,
`XDG_STATE_HOME`, `XDG_CACHE_HOME`, `~/.ssh` et sshd y vivent ; l'environnement du processus est remplacé
(`env` minimal, sans `SSH_AUTH_SOCK`). Le vrai `~/.ssh` n'est jamais lu.

**Config SSH de test** (`fixtures.py`, défaut 50 hôtes) : `Include conf.d/*.conf` (3 fichiers ; 8 pour les gros
volumes), 2 bastions, `ProxyJump` simple et en chaîne (`bastion-lab,bastion-prod`), `Port`, `LocalForward`,
`IdentityFile ~/.ssh/id_%r_ed25519` (tokens), un bloc `Match host … user …`, un `Host *.lab…` et un `Host *`
(`ServerAliveInterval 60` par défaut, réglable par `--alive`). Avec sshd, 5 hôtes `local-1…5` pointent sur un
sshd jetable (clé hôte dans `known_hosts`, clé ed25519 sans passphrase, `IdentitiesOnly yes`). Le compteur
d'en-tête affiche donc 52 hôtes (57 avec sshd).

### TUI : démarrage

`portico` est lancé dans un pty de 120×40 (`openpty`, `TIOCSWINSZ`, terminal de contrôle). Le harnais lit la
sortie brute et horodate chaque bloc **à l'arrivée**, sans l'interpréter (l'interprétation par `pyte` coûte
~10 ms par écran et retarderait la lecture suivante), jusqu'à 500 ms de silence. Les blocs sont ensuite rejoués
dans `pyte` ; on retient l'heure d'arrivée du bloc qui complète chaque jalon, mesurée depuis juste avant le
`fork` :

- **premier rendu** : l'en-tête `PORTICO` est à l'écran ;
- **liste complète** : en-tête `Hosts 52`, dernière ligne de la liste occupée par un hôte (`●`), barre de statut
  (`quit`) : c'est l'écran utilisable.

20 runs. « Cache vidé » : `sync; echo 3 > /proc/sys/vm/drop_caches` avant chaque run. « Cache chaud » : 2 runs
de chauffe écartés. La sortie se fait par `Ctrl-C`.

### TUI : repos (redraw, CPU, RAM)

Après la liste complète (et, avec `--sessions N`, N connexions par `/local-k ↵` puis `Ctrl-Space h`, et 2 s
pour laisser finir toasts et spinners), fenêtre de 30 s : octets et lectures sur le maître du pty (toute
écriture de la TUI y arrive), `utime + stime` de `/proc/<pid>/stat` (tous threads), puis `smaps_rollup`
(RSS, PSS, USS = privé). `--strace` attache `strace -f -c -p <pid>` **pendant la fenêtre seulement** pour
compter tous les appels système de tous les threads. Le délai connexion → invite du shell distant est relevé au
passage.

### TUI : gros volumes

Même enregistrement que le démarrage, avec 200 à 10 000 hôtes répartis sur 8 `Include`. On note si
l'indicateur « Reading ~/.ssh/config… » est apparu. Sonde de réactivité (`--probe`) : dès que l'indicateur est
visible (+200 ms), envoi de `t` (thème suivant, affiché dans l'en-tête) et mesure du délai jusqu'au changement
de thème à l'écran, en vérifiant que la liste n'est pas encore là.

### GUI : première fenêtre

Xvfb 1600×1000×24 démarré une fois ; pour chaque run, `portico-gui` est lancé (`DISPLAY`, `XDG_RUNTIME_DIR`
jetable, `NO_AT_BRIDGE=1`) et une boucle `python-xlib` (pas de 5 ms + coût de la sonde) relève :

- **fenêtre mappée** : une fenêtre visible nommée « Portico » existe (Tauri la peint aussitôt avec
  `backgroundColor` #0b0c0e : c'est une fenêtre sombre vide) ;
- **premier rendu web** : la bande d'en-tête 640×44 (logo, wordmark, onglet Hosts) contient plus de 8 couleurs
  (sonde de ~2 ms) ;
- **liste d'hôtes** : plus de 20 pixels clairs (texte « marbre ») dans la zone de la liste de la barre latérale ;
  le squelette de chargement n'en a aucun.

10 runs (avec un run de chauffe écarté, ou `drop_caches` avant chaque run). Avec n = 10, le p95 (rang le plus
proche) est le maximum. Variante : `WEBKIT_DISABLE_COMPOSITING_MODE=1`.

### GUI : repos et RAM

Après rendu complet et 3 s, puis N connexions au clavier (xdotool : `Alt-0`, `/`, `Ctrl-A`, alias, `↵`, 3 s
chacune), fenêtre de 30 s : `utime + stime` de chaque processus de l'arbre (`portico-gui`,
`WebKitWebProcess`, `WebKitNetworkProcess`), puis `smaps_rollup` de chacun, sommés. Les sessions sont
vérifiées par le nombre de connexions TCP établies vers le port du sshd (`/proc/net/tcp`) et par une capture.

## Résultats

### TUI : démarrage (52 hôtes, 3 `Include`)

| Condition | Premier rendu (méd. / p95 / min–max) | Liste complète (méd. / p95 / min–max) | Objectif 100 ms |
|---|---|---|---|
| Cache vidé (`drop_caches`), 20 runs | 22,3 / 26,8 / 18,9–27,5 ms | **25,1 / 31,1** / 23,1–31,5 ms | ✅ marge 69 ms au p95 |
| Cache chaud, 20 runs | 8,9 / 10,5 / 6,8–11,0 ms | **11,2 / 13,7** / 8,9–14,1 ms | ✅ marge 86 ms au p95 |

Le chargement des 52 hôtes arrive 2 à 3 ms après le premier rendu. Un `strace -tt` montre le premier écran envoyé
en ~20 écritures de 1 Kio (le `stdout` de la std est tamponné par 1 Kio), sans conséquence ici.

### TUI : repos 30 s

| Vue | CPU 30 s | Écritures pty | RSS / PSS / USS fin |
|---|---|---|---|
| Liste d'hôtes (52), 0 session | 0 tick (0 %) | **0 octet** | 9,7 / 7,7 / 7,3 Mo |
| Onglet terminal, 1 session (shell idle) | 1 tick (10 ms, 0,03 %) | **0 octet** | 13,1 / 11,1 / 10,7 Mo |
| Onglet terminal, 5 sessions | 0 tick | **0 octet** | 14,6 / 12,4 / 12,1 Mo |
| 5 sessions, `ServerAliveInterval 15` | 2 ticks (0,07 %) | 250 octets en 10 écritures | 14,6 / 12,3 / 12,1 Mo |
| Liste de 2 000 hôtes, 0 session | 1 tick | 0 octet | 20,4 / 18,2 / 17,9 Mo |

(Mo = Mio ; un tick = 10 ms.) Appels système de tous les threads pendant 30 s (`strace -c`) : liste seule
86 appels, 5 sessions 88, presque tous des `futex`/`restart_syscall` provoqués par l'attachement de strace
et la sortie de threads `spawn_blocking` inactifs : **aucun timer périodique, aucune écriture**. Avec
`ServerAliveInterval 15`, chaque sonde keepalive/latence (5 sessions × 2 en 30 s) déclenche un `draw` : 25
octets par sonde, soit exactement l'épilogue de ratatui (`reset` des couleurs, curseur masqué) quand le diff est
vide, plus un `openat("/dev/tty")` (lecture de la taille). Rien ne change à l'écran (latence « <1 ms »
identique) : c'est un appel de rendu inutile mais quasi gratuit, pas un redraw visible.

Connexion jusqu'à l'invite du shell distant (sshd local, ed25519) : médiane **102 ms** (5 sessions : 87–109 ms).

### TUI : gros volumes (8 `Include`)

| Hôtes | Premier rendu (méd.) | Liste complète (méd. / p95) | Indicateur de chargement vu |
|---|---|---|---|
| 52 (3 `Include`) | 8,9 ms | 11,2 / 13,7 ms | non (trop rapide) |
| 202 | 7,8 ms | 29,6 / 40,8 ms | 5/5 |
| 502 | 8,9 ms | 103 / 112 ms | 5/5 |
| 1 002 | 7,0 ms | 362 / 402 ms | 5/5 |
| 2 002 | 8,1 ms | **1 541 / 1 862 ms** (10 runs) | 10/10 |
| 10 002 | 7,1 ms | **47 088 ms** (1 run) | 1/1 |

Réactivité pendant le chargement de 2 002 hôtes (5 sondes) : la touche `t` change le thème affiché en
**22,6–27,3 ms** (interprétation de l'écran par `pyte` comprise), alors que la liste arrive 1,2 à 1,4 s plus tard.
Le chargement (`Engine::load_hosts`, `spawn_blocking`) ne bloque donc ni la boucle d'événements ni le rendu. En
revanche sa durée croît en O(n²) : ×4 quand le nombre d'hôtes double (500 → 1 000 → 2 000).

### GUI : première fenêtre (52 hôtes)

| Condition (10 runs) | Fenêtre mappée | Premier rendu web | Liste d'hôtes |
|---|---|---|---|
| Cache chaud | **73 / 93 ms** (70–93) | **1 148 / 1 192 ms** (1 109–1 192) | 1 219 / 1 259 ms |
| Cache vidé | **155 / 197 ms** (132–197) | 1 464 / 2 276 ms (1 354–2 276) | 1 536 / 2 285 ms |
| Cache chaud, `WEBKIT_DISABLE_COMPOSITING_MODE=1` | 80 / 105 ms | 1 004 / 1 115 ms | 1 051 / 1 209 ms |

(médiane / p95, min–max.) Chronologie d'un lancement sous `strace -f` (qui ralentit) : connexion X à +150 ms,
`WebKitNetworkProcess` lancé à +510 ms, `WebKitWebProcess` à +785 ms ; le premier rendu suit le chargement et
l'exécution du bundle (694 Ko de JS en un seul chunk, xterm.js compris). La liste d'hôtes suit le premier rendu
de ~70 ms (squelette puis lignes).

### GUI : repos 30 s et RAM

| Vue | CPU 30 s (`portico-gui` / `WebKitWebProcess` / réseau) | Total CPU |
|---|---|---|
| Liste d'hôtes, 0 session | 20 / 20 / 0 ms | **0,13 %** |
| 5 sessions ouvertes, liste d'hôtes affichée (`Alt-0`) | 10 / 10 / 0 ms | **0,07 %** |
| Onglet terminal focalisé, 1 session | 210 / 3 590 / 0 ms | **12,7 %** |
| Onglet terminal focalisé, 5 sessions | 210 / 3 440 / 0 ms | **12,2 %** |

Le coût du terminal ne dépend pas du nombre de sessions et disparaît quand la liste est affichée : il vient de
l'onglet visible et focalisé. Cause probable : `cursorBlink: true`
(`apps/portico-gui/src/lib/components/TerminalView.svelte:255`), donc un repeint du canvas xterm.js toutes les
~0,5 s, coûteux en rendu logiciel (llvmpipe, composition WebKit). Non vérifié en désactivant l'option
(interdit de toucher au code ici).

| État | `portico-gui` RSS / PSS | `WebKitWebProcess` RSS / PSS | `WebKitNetworkProcess` RSS / PSS | **Total RSS / PSS / USS** |
|---|---|---|---|---|
| Liste d'hôtes (52), 0 session | 177 / 104 Mo | 334 / 263 Mo | 47 / 22 Mo | **558 / 388 / 284 Mo** |
| 5 sessions, onglet terminal | 180 / 107 Mo | 491 / 419 Mo | 47 / 21 Mo | **719 / 547 / 444 Mo** |
| 5 sessions, liste affichée | 184 / 110 Mo | 493 / 421 Mo | 47 / 21 Mo | **724 / 552 / 449 Mo** |

(Mo = Mio, fin de la fenêtre de 30 s ; le JSON contient aussi les valeurs juste après le rendu, à ±3 %.) Le
premier terminal xterm.js ajoute ~90 Mo PSS au processus web (1 session : 484 Mo PSS au total), chacun des
suivants ~16 Mo ; le cœur Rust des 5 sessions ajoute ~3 Mo à `portico-gui`.

## Limites des mesures

- **Pas de GPU et pas de bureau.** WebKitGTK sous Xvfb rend en logiciel (Mesa llvmpipe) sans compositeur ;
  premier rendu, CPU du curseur et RAM du processus web (tampons GL logiciels) y sont pessimistes. La mesure
  GUI qui compte pour l'objectif reste à faire sur un poste réel : Linux avec GPU (GNOME/KDE) et surtout macOS
  (WKWebView), où Tauri démarre en général bien plus vite.
- **« À froid » ≠ disque froid.** `drop_caches` vide le cache de pages de la VM invitée, pas celui de l'hôte
  qui sert le disque virtuel ; un premier lancement après redémarrage sur un SSD réel sera un peu plus lent
  que la colonne « cache vidé ». La TUI garde une marge de ×3.
- **Premier rendu TUI** = arrivée des octets dans le pty, pas leur affichage par un vrai émulateur (quelques
  ms de plus dans iTerm2, Alacritty ou GNOME Terminal).
- **Résolution GUI** : boucle de 5 ms plus le coût des sondes (~2 ms pour l'en-tête, ~4 ms pour la barre
  latérale, ~0,3 ms pour trouver la fenêtre) ; la fenêtre est trouvée via l'arbre X (pas de gestionnaire de
  fenêtres).
- **CPU** en ticks de 10 ms (`CLK_TCK` = 100) : sur 30 s, 1 tick = 0,03 %. Les chiffres « 0 » signifient
  « moins d'un tick ».
- **Keepalive** : la config de test fixe `ServerAliveInterval 60` (aucune sonde dans les 30 s) ; le défaut du
  moteur est 15 s (`DEFAULT_KEEPALIVE`), mesuré à part.
- **Charge** : un seul échantillon de config (synthétique) ; sshd local, donc latence réseau nulle. Les
  10 000 hôtes : un seul run.
- **Non mesuré** : GUI sur macOS et sur un bureau Linux réel, latence de frappe, débit du terminal (voir le
  spike de la phase 1c dans `docs/STACK.md`), SFTP et tunnels sous charge, RAM GUI avec 2 000 hôtes.

## Pistes

Par ordre d'impact, sans modification faite ici :

1. **Chargement des hôtes en O(n²)** (2 000 hôtes : 1,5 s ; 10 000 : 47 s). `inventory` résout chaque alias
   en parcourant tous les blocs (normal : sémantique first-wins d'OpenSSH), mais `SshConfig::resolve`
   (`ssh_config/resolve.rs:98-105`) recalcule pour **chaque bloc** le `HostName` courant et clone le `User`
   (deux allocations), même pour les blocs `Host` qui n'en ont pas besoin. Calculer ces valeurs paresseusement
   (seulement pour un bloc `Match`) et sauter d'emblée les blocs `Host` à motifs concrets qui ne nomment pas
   l'alias (index `HashMap` alias → blocs, les blocs à jokers restant évalués) ramènerait la résolution à peu
   près en O(n × blocs à jokers). `host_aliases` déduplique aussi en O(n²) (`out.iter().any`), un `HashSet`
   suffit. Ajouter un test de non-régression (par ex. 2 000 hôtes en moins de 100 ms en release). En attendant,
   l'UI reste réactive, avec l'indicateur de chargement.
2. **Premier rendu GUI** (1,1 s sous Xvfb). D'abord le mesurer sur macOS et sur un bureau Linux avec GPU
   (`bench/perf.py gui-start` fonctionne sur tout serveur X). Ensuite : découper le bundle (charger xterm.js et
   l'addon WebGL à l'ouverture du premier terminal, `import()` dynamique), ce qui allège l'évaluation du JS
   initial ; vérifier que les polices ne retardent pas le premier rendu (`font-display`). La fenêtre native
   apparaît déjà en 73 ms avec la couleur du thème, ce qui évite l'écran blanc.
3. **CPU du terminal GUI au repos** (12 % sous rendu logiciel). Confirmer la cause en désactivant
   `cursorBlink` ; si confirmé : arrêter le clignotement quand la fenêtre perd le focus ou après quelques
   secondes d'inactivité (comme GNOME Terminal), ou le proposer en réglage. À remesurer avec GPU.
4. **Appel de rendu sur une latence inchangée** (TUI) : ne déclencher le `draw` que si la valeur affichée
   change (25 octets et un `openat` par sonde aujourd'hui, invisible mais évitable). Envelopper `stdout` dans un
   `BufWriter` plus grand que 1 Kio réduirait aussi le nombre d'écritures d'un écran complet (~20 → 1).

## Reproduire

```bash
sudo bench/run-all.sh bench/out                    # tout (~15 min), JSON + logs par mesure
python3 bench/perf.py tui-start --bin target/release/portico --runs 20 --drop-caches
python3 bench/perf.py tui-idle  --bin target/release/portico --seconds 30 --sessions 5 --strace
python3 bench/perf.py tui-load  --bin target/release/portico --hosts 2000 --includes 8 --runs 10 --probe 5
python3 bench/perf.py gui-start --bin target/release/portico-gui --runs 10 --warmup 1 --shot /tmp/gui.png
python3 bench/perf.py gui-idle  --bin target/release/portico-gui --seconds 30 --sessions 5
python3 bench/fixtures.py /tmp/cfg --hosts 2000 --includes 8   # la config seule, pour l'inspecter
```
