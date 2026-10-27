# Release de Portico

Ce document décrit comment une version de Portico est construite, signée et publiée,
et comment un utilisateur vérifie ce qu'il télécharge. Le workflow est
[`.github/workflows/release.yml`](../.github/workflows/release.yml).

## Ce qui est livré

| Artefact | Plateformes | Construit par | Signé |
|---|---|---|---|
| `Portico_<v>_amd64.deb`, `Portico_<v>_arm64.deb` | Debian 12+, Ubuntu 22.04+ | Tauri bundler | non (somme SHA256) |
| `Portico_<v>_amd64.AppImage`, `Portico_<v>_aarch64.AppImage` | Linux glibc ≥ 2.35 | Tauri bundler + linuxdeploy | non (somme SHA256) |
| `Portico_<v>_universal.dmg` | macOS 13+ (Apple Silicon et Intel) | Tauri bundler | Developer ID + notarisation **si les secrets existent** (voir plus bas) |
| `Portico_<v>_x64-setup.exe` | Windows 11 x64 (Windows 10 1809+ attendu, non testé) | Tauri bundler (NSIS) | **non** pour l'instant (somme SHA256 ; SmartScreen, voir « Installation ») |
| `portico-<v>-<target>.tar.gz` (TUI) | `x86_64`/`aarch64-unknown-linux-gnu`, `x86_64`/`aarch64-unknown-linux-musl`, `x86_64`/`aarch64-apple-darwin` | `cargo build --release` | non (somme SHA256) |
| `portico-<v>-x86_64-pc-windows-msvc.zip` (TUI) | Windows 11 x64 (Windows Terminal) | `cargo build --release`, runtime C statique (`+crt-static`) | non (somme SHA256) |
| `SHA256SUMS` et un `<fichier>.sha256` par artefact | toutes | job `publish` | — |

Chaque tarball TUI contient un dossier `portico-<v>-<target>/` avec le binaire `portico` et `README.md`
(et les fichiers `LICENSE-MIT` et `LICENSE-APACHE` de la racine du dépôt). Le zip Windows a la même
forme : un seul dossier `portico-<v>-x86_64-pc-windows-msvc/` avec `portico.exe`, `README.md` et les
licences (Scoop et winget en dépendent : `extract_dir`, `RelativeFilePath`).

**Windows, installeur de la GUI** : NSIS **par utilisateur** (`installMode: currentUser`), sans droits
administrateur, dans `%LOCALAPPDATA%\Portico` (raccourci du menu Démarrer, entrée « Applications
installées » sous `HKCU\…\Uninstall\Portico`). Le *bootstrapper* Evergreen de WebView2 est embarqué et
ne s'exécute (silencieusement) que si le runtime manque, ce qui n'arrive pas sous Windows 11. Un
*downgrade* silencieux est refusé (`allowDowngrades: false`). La désinstallation ne touche ni `~/.ssh`
ni les données de Portico, partagées avec la TUI : réglages (`%APPDATA%\Portico\portico\config\`,
itinérants) et données (`%LOCALAPPDATA%\Portico\portico\data\` : vault, enregistrements, journaux,
**locaux**). Ce dernier dossier est sous le dossier d'installation : le désinstalleur de Tauri ne supprime
que ses propres fichiers puis un `RMDir` non récursif (vérifié dans `installer.nsi` de `tauri-bundler`
2.10.3, et par la CI : l'exécutable disparaît, les données restent). La case « supprimer
les données de l'application » ne vise que l'état WebView2 (`%LOCALAPPDATA%\dev.portico.app`). Pas de
MSI (WiX, installation par machine) pour l'instant.

**Quelle archive TUI choisir sous Linux ?** La variante `gnu` est recommandée sur un poste de travail :
elle résout les noms via la glibc, donc via NSS (`/etc/nsswitch.conf`), ce qui couvre mDNS (`hote.local`
avec `nss-mdns`), sssd ou LDAP. La variante `musl` est un binaire **entièrement statique** qui tourne
sur n'importe quelle distribution (Alpine, conteneurs, vieilles glibc), mais ne lit que `/etc/hosts` et le DNS.

**Ordres de grandeur** (0.1.0, Linux x86_64, mesurés le 2026-10-21) : `.deb` 7,1 Mio (binaire
`portico-gui` 17,4 Mio installé), AppImage 81,7 Mio (embarque WebKitGTK), binaire TUI 12,3 Mio (gnu) /
12,4 Mio (musl, statique), tarball TUI ≈ 5 Mio.

## Procédure

1. Vérifier que `main` est vert (CI) et que la phase est close dans `docs/PLAN.md`
   (revue `agent-security` faite, conformément à `CLAUDE.md` §9).
2. Mettre la même version à trois endroits (le job `version check` refuse un écart) :
   - `Cargo.toml` → `[workspace.package] version`
   - `apps/portico-gui/src-tauri/tauri.conf.json` → `version`
   - `apps/portico-gui/package.json` → `version`

   puis `cargo update --workspace` pour rafraîchir `Cargo.lock`.
3. Commit `chore: release vX.Y.Z`, puis tag annoté et push :
   ```bash
   git tag -a vX.Y.Z -m "Portico X.Y.Z"
   git push origin main vX.Y.Z
   ```
4. Le workflow construit tout en parallèle (7 jobs TUI dont Windows, 2 jobs GUI Linux, 1 job
   `gui-windows`, 1 job `gui-macos-build`, tous sans secret), puis `gui-macos-sign` attend
   l'**approbation d'un reviewer** de l'environnement
   `release`, signe et notarise l'app ; enfin le job `publish` calcule les sommes et crée une
   **Release brouillon** contenant tous les fichiers.
5. Relire le brouillon : notes de version, présence des 13 artefacts (2 deb, 2 AppImage, 1 dmg,
   1 installeur NSIS, 6 tarballs, 1 zip), de leurs `.sha256` et de `SHA256SUMS`, mention
   `-unsigned` ou `-not-notarized` dans le nom du dmg (voir plus bas). Publier à la main
   (une Release ni brouillon ni *pre-release*).
6. La publication déclenche la **distribution** (tap Homebrew, AUR, bucket Scoop, manifestes winget,
   voir « Distribution ») : fusionner les PR ouvertes sur le tap et sur le bucket, vérifier les deux
   paquets sur l'AUR, ouvrir à la main la PR winget.

**Essai à blanc** : *Actions → Release → Run workflow* (`workflow_dispatch`, champ `distribute`
vide) construit tous les artefacts sans créer de Release et **sans signer** (le job de signature ne
tourne que sur un push de tag) ; ils restent attachés à l'exécution (90 jours). Lancé sur une branche
(*Use workflow from*), c'est aussi le moyen d'obtenir l'installeur Windows et le zip TUI d'une PR pour un
essai sur un PC (artefacts `gui-windows-x64` et `tui-x86_64-pc-windows-msvc`).

**Construire localement** (Linux, `libwebkit2gtk-4.1-dev` installé) :
```bash
cd apps/portico-gui && pnpm install --frozen-lockfile
APPIMAGE_EXTRACT_AND_RUN=1 pnpm tauri build --bundles deb,appimage   # → target/release/bundle/
cargo build --release --locked -p portico-tui                        # → target/release/portico
```

**Construire localement sous Windows** (Visual Studio Build Tools avec « Développement Desktop en C++ »,
Rust `stable-x86_64-pc-windows-msvc`, Node 22 et pnpm ; WebView2 est déjà là sous Windows 11) :
```powershell
cd apps\portico-gui; pnpm install --frozen-lockfile
pnpm tauri build --bundles nsis          # → target\release\bundle\nsis\Portico_<v>_x64-setup.exe
$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo build --release --locked -p portico-tui   # → target\release\portico.exe
```
Sans les épingles du workflow, le bundler télécharge lui-même NSIS (vérifié par SHA-1) et le
*bootstrapper* WebView2 dans `%LOCALAPPDATA%\tauri\`.

## Secrets (macOS uniquement ; Windows : pas encore de signature)

Aucun secret n'est dans le dépôt, ni au niveau du dépôt dans GitHub : ils vivent **uniquement** dans
l'environnement GitHub `release`, que seul le job `gui-macos-sign` référence.

**Mise en place (une fois, par un administrateur du dépôt)** :

1. *Settings → Environments → New environment* : `release`.
2. *Deployment branches and tags* : **Selected branches and tags**, règle de **tag** `v*` (aucune branche).
3. *Required reviewers* : au moins une personne (idéalement pas celle qui pousse le tag), et cocher
   *Prevent self-review* si l'équipe le permet.
4. *Environment secrets* : y créer les six secrets ci-dessous. **Ne pas** les créer dans
   *Settings → Secrets and variables → Actions → Repository secrets* (ils seraient lisibles par
   n'importe quel job de n'importe quel workflow, y compris un workflow modifié dans une branche).

| Secret | Contenu |
|---|---|
| `APPLE_CERTIFICATE` | Certificat **Developer ID Application** + clé privée exportés en `.p12`, encodés en base64 (`base64 -i cert.p12 \| pbcopy`) |
| `APPLE_CERTIFICATE_PASSWORD` | Mot de passe du `.p12` |
| `APPLE_SIGNING_IDENTITY` | Nom de l'identité, ex. `Developer ID Application: Nom (TEAMID)` |
| `APPLE_ID` | Apple ID du compte développeur (notarisation) |
| `APPLE_PASSWORD` | Mot de passe **spécifique à l'app** de cet Apple ID (pas le mot de passe du compte) |
| `APPLE_TEAM_ID` | Identifiant d'équipe (10 caractères) |

**Séparation build / signature** :

- `gui-macos-build` (aucun secret, `APPLE_SIGNING_IDENTITY: "-"`) construit l'app universelle avec une
  signature *ad hoc* et le runtime renforcé, produit le dmg `-unsigned` et une archive `tar` de
  `Portico.app` (artefact `gui-macos-app`, conservé 3 jours). C'est le seul job macOS qui exécute
  cargo, pnpm et les dépendances npm.
- `gui-macos-sign` (`needs: gui-macos-build`, `environment: release`, uniquement sur push d'un tag)
  n'exécute **aucun outil tiers** : il récupère l'archive, `Entitlements.plist` depuis le tag, crée un
  trousseau temporaire (mot de passe aléatoire), importe le `.p12`, signe le code imbriqué puis l'app
  (`codesign --force --options runtime --timestamp --entitlements packaging/macos/Entitlements.plist`),
  notarise l'app (`xcrun notarytool submit --wait`, statut `Accepted` exigé, journal affiché sinon),
  l'agrafe (`xcrun stapler staple`), crée le dmg (`hdiutil create`), le signe, le notarise et l'agrafe
  à son tour, puis vérifie (`codesign --verify --strict`, `spctl --assess`, `stapler validate`).
  Le trousseau est supprimé à la fin, même en cas d'échec (`if: always()`).
- `publish` garde le dmg signé quand il existe, sinon le dmg `-unsigned`. Si la signature échoue,
  **rien n'est publié** (pas de repli silencieux vers un dmg non signé).

Le job de signature choisit son mode selon les secrets présents dans l'environnement :

| Secrets présents | Résultat | Nom du dmg |
|---|---|---|
| les six | `.app` signée Developer ID, runtime renforcé, **notarisée et agrafée** ; dmg signé, notarisé et agrafé | `Portico_<v>_universal.dmg` |
| les trois de signature seulement | `.app` et dmg signés, **non notarisés** (Gatekeeper avertira) ; annotation `warning` | `Portico_<v>_universal-not-notarized.dmg` |
| aucun | le job ne signe rien ; la Release garde le dmg *ad hoc* du build ; annotation `warning` | `Portico_<v>_universal-unsigned.dmg` |

Le mode retenu figure aussi dans le résumé du job. Sans signature, l'utilisateur doit autoriser l'app
une fois : *Réglages Système → Confidentialité et sécurité → Ouvrir quand même* (depuis macOS 15, le clic droit →
Ouvrir ne suffit plus), ou `xattr -dr com.apple.quarantine /Applications/Portico.app`.

## Chaîne d'approvisionnement du workflow

- **Actions épinglées par SHA complet** (version en commentaire), dans `release.yml` et `ci.yml`,
  choisies parmi des versions publiées depuis plus de deux semaines. Pour mettre à jour :
  `git ls-remote https://github.com/<owner>/<repo> refs/tags/<tag>` (prendre le commit pelé `^{}`
  d'un tag annoté). `dtolnay/rust-toolchain` n'a pas de tags de version : on épingle un commit de
  `master` et on passe `toolchain: stable`.
- **`persist-credentials: false`** sur chaque `actions/checkout` : le jeton n'est pas laissé dans
  `.git/config` pour les étapes suivantes.
- **Aucun cache restauré dans `release.yml`** (ni `rust-cache`, ni cache pnpm de `setup-node`,
  `package-manager-cache: false`) : un build de release part des lockfiles (`--locked`,
  `--frozen-lockfile`) et jamais d'un état qu'une exécution de PR aurait pu empoisonner. La CI garde
  ses caches.
- **pnpm en version exacte** (`10.28.0`), Node 22.
- **Outillage AppImage épinglé et vérifié** : le Tauri bundler (`@tauri-apps/cli` 2.12.3, soit
  `tauri-bundler` 2.10.3) ne télécharge un outil que si son fichier **manque** dans
  `${XDG_CACHE_HOME:-~/.cache}/tauri/` (vérifié dans le code de `tauri-bundler` 2.10.3,
  `bundle/linux/appimage/linuxdeploy.rs` : test `exists()` sur chaque fichier). Le job `gui-linux`
  préremplit donc ce dossier avec des copies vérifiées par `sha256sum -c` :

  | Fichier dans le cache | Source épinglée |
  |---|---|
  | `AppRun-<arch>` | `tauri-apps/binary-releases`, release `apprun-old` |
  | `linuxdeploy-07333c6-<arch>.AppImage` | `tauri-apps/binary-releases`, release `linuxdeploy-07333c6` |
  | `linuxdeploy-plugin-appimage.AppImage` | `linuxdeploy/linuxdeploy-plugin-appimage`, release datée `1-alpha-20250213-1` (au lieu de `continuous`) |
  | runtime de type 2 (hors cache) | `AppImage/type2-runtime`, release datée `20251108`, passé à appimagetool par `LDAI_RUNTIME_FILE` |

  Sans ce runtime, appimagetool (embarqué dans le plugin, et aussi dans linuxdeploy) télécharge
  `type2-runtime/releases/download/continuous/runtime-<arch>` à chaque build ; avec
  `LDAI_RUNTIME_FILE`, le plugin passe `--runtime-file` et rien n'est téléchargé. Vérifié localement
  (x86_64) : linuxdeploy `07333c6` + plugin `1-alpha-20250213-1` produisent une AppImage **sans
  réseau** (`unshare -n`) avec `LDAI_RUNTIME_FILE`, et échouent sans (tentative de téléchargement du
  runtime `continuous`). Le workflow vérifie ensuite que le journal du bundle (lancé en `--verbose`,
  cargo hors ligne après `cargo fetch --locked`) ne contient **aucun téléchargement** ni URL de
  release `continuous`, et que `--runtime-file` y figure ; une étape refuse aussi une autre version de
  `@tauri-apps/cli` que celle pour laquelle ces noms et empreintes ont été vérifiés. À chaque mise à
  jour de la CLI Tauri : relire `linuxdeploy.rs` de la version de `tauri-bundler` correspondante,
  retélécharger, recalculer les empreintes, puis mettre `EXPECTED_TAURI_CLI` à jour.

- **Outillage NSIS épinglé et vérifié** (job `gui-windows`) : `tauri-bundler` 2.10.3 ne télécharge NSIS
  que si `%LOCALAPPDATA%\tauri\NSIS` manque ou est incomplet (fichiers requis, `NSIS.exe` et
  `nsis_tauri_utils.dll` contrôlés par SHA-1 : `bundle/windows/nsis/mod.rs`). Le job préremplit ce
  dossier, dans la disposition que le bundler construit lui-même, à partir de copies vérifiées par
  SHA-256 :

  | Fichier | Source épinglée | SHA-256 |
  |---|---|---|
  | `nsis-3.13.zip` (→ `NSIS\`) | `tauri-apps/binary-releases`, release `nsis-3.13` | `ba63dffc4410ee89193e1cb5a41989991bd77c61068da17e3156d136b7b0b3d8` |
  | `nsis_tauri_utils.dll` (→ `NSIS\Plugins\x86-unicode\additional\`) | `tauri-apps/nsis-tauri-utils`, release `nsis_tauri_utils-v0.5.3` | `5ba143b5db4a87d32d6e7802e033330aae56cbceabe0d1e3ba41948385ad4709` |

  Les SHA-1 de ces deux fichiers, et celui de `NSIS.exe` dans l'archive, ont été recalculés le
  2026-10-24 et sont **identiques** aux constantes du bundler (`NSIS_SHA1`, `NSIS_TAURI_UTILS_SHA1`,
  `NSIS_EXE_SHA1`) : les épingles SHA-256 désignent bien les fichiers que Tauri attend. Le bundle tourne
  ensuite cargo hors ligne, et le journal (`--verbose`) ne doit contenir aucun téléchargement ni
  « Verifying NSIS package » / « Recreating » / « Redownloading ». Même contrôle de version de
  `@tauri-apps/cli` que pour l'AppImage.
- **Bootstrapper WebView2** (`webviewInstallMode: embedBootstrapper`) : le bundler réutilise
  `%LOCALAPPDATA%\tauri\MicrosoftEdgeWebview2Setup.exe` s'il existe. Microsoft met ce fichier à jour
  sur place (pas d'empreinte fixe possible) : le job le télécharge depuis le lien officiel
  (`go.microsoft.com/fwlink/p/?LinkId=2124703`) et exige une **signature Authenticode valide** au nom de
  `CN=Microsoft Corporation` ; empreinte et signataire vont dans le résumé du job. L'alternative
  `downloadBootstrapper` le ferait télécharger **à l'installation**, chez l'utilisateur, par le
  greffon `NSISdl` sans cette vérification.
- **Installation de bout en bout sur le runner** : installation silencieuse (`/S`, sans droits
  administrateur), clé `HKCU\…\Uninstall\Portico` présente avec la bonne version (celle que nomme le
  `ProductCode` winget), lancement de l'app (elle doit tenir 10 s : WebView2, durcissement du processus,
  moteur), puis désinstallation silencieuse.
- **TUI Windows** : `RUSTFLAGS=-C target-feature=+crt-static` (pas de dépendance au redistribuable
  VC++) ; test de fumée `portico.exe --version` et `--help`, et contrôle des imports DLL
  (`llvm-readobj --coff-imports`) : ni `vcruntime*.dll` ni `msvcp*.dll`.

  **Non vérifié hors CI** : le build Tauri complet avec le cache prérempli (le chemin du cache et les
  noms de fichiers sont lus dans le code du bundler, et l'outillage a été exercé seul, mais pas le
  `pnpm tauri build` de bout en bout dans cet environnement), et toute la partie aarch64 (empreintes
  calculées sur les fichiers téléchargés, outils non exécutés).

### Ce qui est signé, et comment

- **macOS** : l'application (`Portico.app`) est signée avec le runtime renforcé, **sans App Sandbox**,
  avec un seul droit : `com.apple.security.network.client`
  ([`packaging/macos/Entitlements.plist`](../packaging/macos/Entitlements.plist)). Aucune exception du runtime
  renforcé (pas de JIT, pas de mémoire exécutable non signée, pas de variables `DYLD_*`, pas de
  désactivation de la validation des bibliothèques). Le sandbox reste désactivé parce que Portico doit lire
  `~/.ssh` et joindre le socket de `ssh-agent` ; le droit réseau est donc déclaratif tant qu'il n'y a pas de sandbox.
  L'`Info.plist` ajoute `NSLocalNetworkUsageDescription` (macOS 15 demande l'accord avant de joindre un hôte du réseau local).
  L'app est notarisée et agrafée (Gatekeeper la vérifie même hors ligne une fois copiée dans
  *Applications*) ; le `.dmg` qui la contient est lui aussi signé, notarisé et agrafé.
- **Linux** : `.deb`, AppImage et tarballs ne sont pas signés cryptographiquement. L'intégrité repose sur
  `SHA256SUMS` publié dans la Release GitHub (canal HTTPS de GitHub). Pas de dépôt APT signé pour l'instant.
- **TUI macOS** : binaires non signés (un binaire en ligne de commande téléchargé par `curl` n'est pas mis
  en quarantaine ; via un navigateur : `xattr -d com.apple.quarantine portico`).
- **Windows** : installeur NSIS, `portico-gui.exe` et `portico.exe` **non signés** (Authenticode) tant
  qu'aucun certificat n'est choisi (ADR « Signature Windows », `docs/STACK.md` : Azure Trusted Signing
  ou certificat OV). L'intégrité repose sur `SHA256SUMS`. Conséquences : SmartScreen avertit au premier
  lancement de l'installeur téléchargé, et **Smart App Control** (Windows 11), s'il est actif, bloque
  les binaires non signés sans recours (voir « Installation »). Le futur job `gui-windows-sign` est
  décrit dans `release.yml` (job `gui-windows`) : environnement `release`, tags seulement, signature de
  l'exécutable puis de l'installeur par `bundle.windows.signCommand`, comme `gui-macos-sign`.

## Vérifier un téléchargement

```bash
# Tous les fichiers téléchargés dans le même dossier que SHA256SUMS :
sha256sum --ignore-missing -c SHA256SUMS        # Linux
shasum -a 256 --ignore-missing -c SHA256SUMS    # macOS

# Un seul fichier :
sha256sum -c portico-0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256

# macOS : signature, notarisation, droits
codesign --verify --deep --strict --verbose=2 /Applications/Portico.app
spctl --assess --type execute --verbose=4 /Applications/Portico.app   # « source=Notarized Developer ID »
codesign --display --entitlements - /Applications/Portico.app

# Debian/Ubuntu : contenu et dépendances avant installation
dpkg-deb -I Portico_0.1.0_amd64.deb && dpkg-deb -c Portico_0.1.0_amd64.deb
sudo apt install ./Portico_0.1.0_amd64.deb
```

```powershell
# Windows (PowerShell) : comparer à la ligne de SHA256SUMS (ou au fichier .sha256)
(Get-FileHash .\Portico_0.5.0_x64-setup.exe -Algorithm SHA256).Hash.ToLower()
Get-Content .\Portico_0.5.0_x64-setup.exe.sha256
# Signature Authenticode (aujourd'hui : NotSigned)
Get-AuthenticodeSignature .\Portico_0.5.0_x64-setup.exe
```

## Installation

- **deb** : `sudo apt install ./Portico_<v>_amd64.deb` (installe `/usr/bin/portico-gui`, l'entrée de menu
  `Portico` et les icônes `hicolor`). Dépendances : `libwebkit2gtk-4.1-0` et `libgtk-3-0` (ajoutées par
  le bundler), `libdbus-1-3` (liée directement par `tao`) et `libc6 (>= 2.35)` (plancher du runner
  Ubuntu 22.04) ; les autres bibliothèques liées (soup, JavaScriptCore, cairo, gdk-pixbuf, glib) sont des
  dépendances de WebKitGTK et GTK. Liste vérifiée avec `dpkg-shlibdeps`.
- **AppImage** : `chmod +x Portico_<v>_amd64.AppImage && ./Portico_<v>_amd64.AppImage`
  (FUSE 2 requis, sinon `--appimage-extract-and-run`).
- **dmg** : glisser `Portico.app` dans *Applications*.
- **TUI** : `tar xzf portico-<v>-<target>.tar.gz && install -m 0755 portico-<v>-<target>/portico ~/.local/bin/`.
- **Homebrew** (macOS, Linux) : `brew install <owner>/portico/portico` (TUI) et
  `brew install --cask <owner>/portico/portico` (GUI, macOS 13+). Le nom complet évite l'ambiguïté
  entre la formule et le cask, qui portent le même nom, et vaut accord de confiance pour ce paquet
  (Homebrew 7 ignore sinon les taps tiers non approuvés : `brew trust <owner>/portico`).
- **AUR** (Arch Linux) : `portico-bin` (TUI) et `portico-gui-bin` (GUI), par exemple
  `yay -S portico-bin portico-gui-bin`, ou `git clone https://aur.archlinux.org/portico-bin.git && cd portico-bin && makepkg -si`.
- **Windows, GUI** : lancer `Portico_<v>_x64-setup.exe` (aucun droit administrateur). Comme
  l'installeur n'est pas signé, **SmartScreen** affiche « Windows a protégé votre ordinateur » au premier
  lancement d'un fichier téléchargé par un navigateur : après avoir vérifié la somme (ci-dessus),
  *Informations complémentaires → Exécuter quand même* (en anglais : *More info → Run anyway*). C'est
  l'équivalent du « Ouvrir quand même » de macOS ; l'app installée ne redemande rien.
- **Windows, TUI** : extraire le zip (dans l'Explorateur : *Propriétés → Débloquer* avant, ou
  `Unblock-File .\portico-<v>-x86_64-pc-windows-msvc.zip` en PowerShell, pour retirer la marque
  « téléchargé d'Internet »), placer `portico.exe` dans un dossier du `PATH`, et lancer `portico` dans
  **Windows Terminal** (la console historique `conhost` fonctionne, avec un rendu moins fidèle).
- **Scoop** (TUI) : `scoop bucket add portico https://github.com/<owner>/scoop-portico`, puis
  `scoop install portico/portico` ; mise à jour par `scoop update portico`. Scoop vérifie la somme du
  manifeste et n'ajoute pas de marque « Internet » : pas d'invite SmartScreen.
- **winget** (dès que les manifestes sont acceptés dans `microsoft/winget-pkgs`) :
  `winget install fs0ciety7000.Portico` (GUI) et `winget install fs0ciety7000.PorticoTUI` (TUI,
  commande `portico`).
- **Smart App Control** (Windows 11, *Sécurité Windows → Contrôle des applications et du navigateur*) :
  s'il est **activé**, il bloque les exécutables non signés sans réputation, sans bouton de
  contournement ; Portico ne peut alors pas tourner tant qu'il n'est pas signé. En mode « évaluation »
  ou désactivé, rien ne change. Portico ne demande jamais de le désactiver.

Les secrets restent dans le trousseau système : sous Linux, un service Secret Service (GNOME Keyring,
KeePassXC, KWallet ≥ 5.97…) doit tourner dans la session ; à défaut, Portico propose son vault chiffré.
Sous Windows, c'est le **Gestionnaire d'identification** (*Credential Manager*, identifiants
génériques), toujours disponible. L'agent SSH est celui d'**OpenSSH pour Windows** (service
`ssh-agent`, tube `\\.\pipe\openssh-ssh-agent`, ou `SSH_AUTH_SOCK` s'il désigne un tube) ; Pageant
(PuTTY) n'est pas pris en charge.

## Distribution (Homebrew, AUR, Scoop, winget)

Les paquets sont **générés depuis la Release publiée** : mêmes fichiers, mêmes sommes que `SHA256SUMS`,
rien n'est reconstruit.

| Cible | Paquet | Source | Gabarit et rendu |
|---|---|---|---|
| Tap `<owner>/homebrew-portico` | formule `portico` (TUI) | tarballs `aarch64`/`x86_64-apple-darwin` et `aarch64`/`x86_64-unknown-linux-gnu` (`on_macos`/`on_linux`, `on_arm`/`on_intel`) | [`packaging/homebrew/`](../packaging/homebrew/) |
| Tap `<owner>/homebrew-portico` | cask `portico` (GUI) | `Portico_<v>_universal*.dmg`, `depends_on macos: :ventura` (= 13 ou plus, comme `minimumSystemVersion`) | idem |
| AUR | `portico-bin` (TUI) | tarballs `x86_64`/`aarch64-unknown-linux-gnu` | [`packaging/aur/`](../packaging/aur/) |
| AUR | `portico-gui-bin` (GUI) | `.deb` `amd64`/`arm64`, extrait | idem |
| Bucket Scoop `<owner>/scoop-portico` | `portico` (TUI) | zip `x86_64-pc-windows-msvc` (`extract_dir`, *shim* `portico.exe`) | [`packaging/scoop/`](../packaging/scoop/) |
| `microsoft/winget-pkgs` (PR **à la main**) | `fs0ciety7000.Portico` (GUI) | `Portico_<v>_x64-setup.exe`, type `nullsoft`, `Scope: user`, `ProductCode: Portico` | [`packaging/winget/`](../packaging/winget/) |
| idem | `fs0ciety7000.PorticoTUI` (TUI) | zip, `NestedInstallerType: portable`, alias `portico` | idem |

- **AUR, GUI : le `.deb` plutôt que l'AppImage.** Le `.deb` ne contient que `portico-gui`, l'entrée de
  menu et les icônes ; il se lie au WebKitGTK d'Arch (`webkit2gtk-4.1`), donc les correctifs de sécurité
  de WebKit arrivent par pacman. L'AppImage embarque le WebKitGTK d'Ubuntu 22.04 (figé, 82 Mio) et
  demande FUSE. Le binaire, construit sur glibc 2.35, tourne sur la glibc plus récente d'Arch (ABI
  stable, sonames identiques). Dépendances déclarées = bibliothèques liées (`readelf -d`), chacune
  rattachée à son paquet Arch : `cairo dbus gdk-pixbuf2 glib2 glibc gtk3 libgcc libsoup3 webkit2gtk-4.1`,
  plus `hicolor-icon-theme` (icônes). TUI : `glibc libgcc`. Optionnels : `org.freedesktop.secrets`
  (gnome-keyring, keepassxc…) et `openssh` (`ssh-agent`).
- `provides`/`conflicts` : `portico` et `portico-gui` (pour un futur paquet construit depuis les sources).
  Licence SPDX `MIT OR Apache-2.0` ; `LICENSE-MIT` et `LICENSE-APACHE` dans `/usr/share/licenses/<paquet>/`
  (pour la GUI, téléchargés depuis le tag : leurs sommes viennent des fichiers du dépôt au tag).
- **Cask non notarisé** : `render.sh` prend le dmg que la Release a gardé. S'il porte `-unsigned` ou
  `-not-notarized`, le cask pointe vers lui et ajoute un `caveats` (Gatekeeper bloque le premier
  lancement : autorisation dans *Réglages Système → Confidentialité et sécurité*, ou
  `xattr -dr com.apple.quarantine`), et le job émet un avertissement. Le dépôt officiel `homebrew/cask`
  refuserait une app non notarisée ; un tap tiers l'accepte.
- **`zap`** (`brew uninstall --zap --cask portico`) supprime `~/Library/Application Support/dev.Portico.portico`
  (`ProjectDirs::from("dev", "Portico", "portico")` de `store.rs` : `hosts.toml`, réglages, snippets,
  vault, journaux, **partagés avec la TUI**) et l'état WebKit/AppKit de `dev.portico.app` (`Caches`,
  `HTTPStorages`, `Preferences`, `Saved Application State`, `WebKit`). Jamais `~/.ssh`. Les entrées du
  trousseau (service `dev.portico`) restent : *Trousseaux d'accès* pour les retirer.

### Déroulé

1. La publication de la Release (`release: published` ; un brouillon ne déclenche rien, une
   *pre-release* est ignorée) lance cinq jobs, et **aucun build** :
   - `dist-render` (aucun secret, `contents: read`) : vérifie que la Release est publiée et finale
     (`vX.Y.Z`, ni brouillon ni *pre-release*), télécharge les 9 artefacts utiles (4 tarballs, 2 `.deb`,
     le dmg, le zip Windows, l'installeur NSIS) et `SHA256SUMS`, les vérifie (`sha256sum --strict -c`),
     rend les fichiers (chaque `render.sh` refuse un artefact absent de `SHA256SUMS` ou une somme mal
     formée), contrôle la syntaxe (`ruby -c`, `bash -n`, `jq` pour Scoop, YAML pour winget) et les
     transmet dans l'artefact `distribution` (7 jours) ; les manifestes winget sont aussi attachés à
     part (artefact `winget-manifests`, 30 jours) ;
   - `homebrew` (`permissions: {}`, secret `HOMEBREW_TAP_TOKEN`) : pousse `Formula/portico.rb` et
     `Casks/portico.rb` sur la branche `portico-<v>` du tap et **ouvre une PR** (ou met à jour celle qui
     existe). La PR laisse au propriétaire le dernier mot, et à la CI du tap (`brew test-bot`) le temps
     de tourner ;
   - `scoop` (`permissions: {}`, secret `SCOOP_BUCKET_TOKEN`) : pousse `bucket/portico.json` sur la
     branche `portico-<v>` du bucket et **ouvre une PR** (ou met à jour celle qui existe), exactement
     comme `homebrew` ;
   - `aur` (`permissions: {}`, secret `AUR_SSH_PRIVATE_KEY`) : pousse `PKGBUILD` et `.SRCINFO` sur
     `master` de `portico-bin` et `portico-gui-bin` (commit `Update to <v>-<pkgrel>`). L'AUR n'a pas de
     PR. La clé d'hôte de `aur.archlinux.org` n'est acceptée que si son empreinte ed25519 est celle
     publiée sur la page d'accueil de l'AUR (`SHA256:RFzBCUItH9LZS0cKB5UE6ceAYhBD5C8GeOBip8Z11+4`).
   - `site` (`permissions: {}`, environnement `website`, secret `SITE_DISPATCH_TOKEN`) : envoie un
     `repository_dispatch` (`portico-release`, avec le tag) au dépôt du site, `<owner>/Portico-show`
     (autre nom : variable `SITE_REPO`). Le site se resynchronise aussitôt (version, commandes
     d'installation, paquets Arch et Scoop servis en attendant l'AUR et le bucket, statuts de
     `docs/PLAN.md`) au lieu d'attendre sa synchronisation périodique. Une seule requête `curl`, aucun
     code du dépôt ; sans jeton, avertissement et le site suit à son rythme.
2. Les jobs à secret n'exécutent **aucun code du dépôt** ni action tierce (sauf
   `download-artifact`) : `git`, `gh` et `ssh` du runner, sur les fichiers rendus. Les jetons du tap et du bucket passent
   en en-tête HTTP de chaque commande (`http.extraheader`), jamais dans une URL ni dans `.git/config` ;
   la clé AUR est écrite en `0600` dans `$RUNNER_TEMP` et supprimée en fin de job (`if: always()`).
3. **Sans secret, le job se saute proprement** : avertissement et ligne dans le résumé, statut vert.
   Rien n'est relancé tant qu'aucun secret n'est fourni.

**Relancer** (secret ajouté après coup, PR fermée par erreur…) : *Actions → Release → Run workflow*
depuis `main`, champ `distribute` = `vX.Y.Z` (une Release **publiée** ; ce mode ne construit rien).
Sur `release: published`, GitHub exécute le workflow **tel qu'il est au tag** : la distribution
automatique commence avec le premier tag qui contient ces jobs ; une version antérieure (`v0.1.0`) se
distribue par ce lancement manuel. Le contenu inchangé n'est pas recommité (relance idempotente).
Pour republier la même version avec un `PKGBUILD` corrigé, rendre localement avec `PKGREL=2` et pousser
à la main (le workflow utilise toujours `pkgrel=1`).

### Mise en place (une fois, par le propriétaire)

1. **Tap** : créer le dépôt **public** `<owner>/homebrew-portico` (Homebrew trouve `<owner>/portico` à
   ce nom), **non vide** (la PR a besoin d'une branche de base) : par exemple `brew tap-new <owner>/portico`,
   qui ajoute un README et la CI `brew test-bot`, puis pousser ce dépôt. Un autre nom : variable
   `HOMEBREW_TAP_REPO` (`owner/repo`) dans *Settings → Secrets and variables → Actions → Variables*.
2. **Jeton du tap** : *Settings → Developer settings → Fine-grained tokens*, propriétaire de la
   ressource = celui du tap, *Repository access : Only select repositories* → `homebrew-portico` seul,
   permissions **Contents : Read and write** et **Pull requests : Read and write** (Metadata en lecture,
   imposé), expiration courte (à renouveler). Pas de *classic token*.
3. **AUR** : un compte sur <https://aur.archlinux.org> (idéalement dédié à Portico : une clé SSH du compte
   peut pousser **tous** ses paquets), une clé réservée à la CI :
   ```bash
   ssh-keygen -t ed25519 -N '' -C portico-release-ci -f aur-portico
   ```
   `aur-portico.pub` dans *My Account → SSH Public Key* ; le contenu de `aur-portico` devient le secret,
   puis supprimer le fichier local. Le premier push crée les deux paquets (noms libres à ce jour).
   Variables facultatives : `AUR_MAINTAINER` (`Nom <courriel>`, ligne `# Maintainer:`),
   `AUR_GIT_NAME`, `AUR_GIT_EMAIL` (auteur des commits AUR).
4. **Environnement `distribution`** (*Settings → Environments*) : *Deployment branches and tags* →
   **Selected** : règle de tag `v*` (événement `release`) et branche `main` (relance manuelle). Y créer
   `HOMEBREW_TAP_TOKEN` et `AUR_SSH_PRIVATE_KEY` ; **jamais** en secrets de dépôt. **Required reviewers
   obligatoires**, comme pour `release` (*Prevent self-review* si l'équipe le permet) : sans eux, quiconque peut pousser un
   tag `v*` fait tourner un `release.yml` de son choix avec ces secrets, et la clé AUR publie sans revue
   chez les utilisateurs Arch. Chaque distribution attend donc une approbation.
5. **Tags protégés** (*Settings → Rules → Rulesets*, cible *Tags*, motif `v*`) : création, mise à jour et
   suppression réservées aux administrateurs (*bypass list*). Cela protège `release` comme `distribution`.
6. **Bucket Scoop** : créer le dépôt **public** `<owner>/scoop-portico` (autre nom : variable
   `SCOOP_BUCKET_REPO`), **non vide** (un README sur `main`), avec un dossier `bucket/` (il est créé par
   la première PR sinon). Jeton *fine-grained* restreint à ce seul dépôt, **Contents** et **Pull
   requests** en lecture-écriture, expiration courte ; secret `SCOOP_BUCKET_TOKEN` dans l'environnement
   `distribution`. Sans lui, le job se saute (avertissement), comme les autres.
7. **winget** (pas d'automatisation : il faudrait un jeton sur un *fork* de `microsoft/winget-pkgs`) :
   à chaque Release publiée,
   1. télécharger l'artefact `winget-manifests` de l'exécution `dist-render` (ou rendre localement :
      `packaging/winget/render.sh X.Y.Z SHA256SUMS out/winget`, `RELEASE_DATE=AAAA-MM-JJ`) ;
   2. sur un PC Windows : `winget validate --manifest out\winget\manifests\f\fs0ciety7000\Portico\X.Y.Z`
      (idem pour `PorticoTUI`), puis `winget settings --enable LocalManifestFiles` et
      `winget install --manifest <dossier>` pour un essai réel (désinstaller ensuite) ;
   3. *fork* de `microsoft/winget-pkgs`, copier les deux dossiers `manifests/f/fs0ciety7000/…/X.Y.Z/`
      dans le fork, une branche et une PR **par paquet** (règle du dépôt), en suivant son modèle de PR.
      Le pipeline de validation de Microsoft (installation, analyse antivirus) tourne sur la PR ; un
      installeur non signé est accepté mais peut être retenu plus longtemps. `wingetcreate update`
      (outil de Microsoft) peut faire les mêmes étapes à partir des URL de la Release.
   Le premier envoi crée les paquets ; l'identifiant (`WINGET_PUBLISHER`, `fs0ciety7000` par défaut) ne
   doit plus changer ensuite.
8. **Site web** (facultatif, déclenchement immédiat) : jeton *fine-grained* dont le propriétaire de la
   ressource est celui du site, *Only select repositories* → `Portico-show` seul, permission
   **Contents : Read and write** (celle qu'exige `repository_dispatch` ; Metadata en lecture, imposé),
   expiration courte. Secret `SITE_DISPATCH_TOKEN` dans un environnement **`website`** (*Settings →
   Environments*, règle de tag `v*`). Pas de reviewers requis : le jeton ne touche que le dépôt du site,
   et le site ne fait que relire la Release publiée et `docs/PLAN.md`.

### Vérifications

```bash
# Rendu depuis le SHA256SUMS d'une Release (gh release download vX.Y.Z -p SHA256SUMS) :
packaging/homebrew/render.sh X.Y.Z SHA256SUMS out/homebrew
packaging/aur/render.sh X.Y.Z SHA256SUMS out/aur
ruby -c out/homebrew/Formula/portico.rb out/homebrew/Casks/portico.rb
packaging/scoop/render.sh X.Y.Z SHA256SUMS out/scoop && jq . out/scoop/bucket/portico.json
RELEASE_DATE=2026-10-24 packaging/winget/render.sh X.Y.Z SHA256SUMS out/winget
# Scoop (PowerShell, Windows) : manifeste local, sans bucket
#   scoop install .\out\scoop\bucket\portico.json ; portico --version ; scoop uninstall portico
# Homebrew (dans un tap local : brew tap-new local/portico, fichiers copiés dedans) :
brew style local/portico
brew audit --strict --new --os all --arch all --formula local/portico/portico
brew audit --strict --new --cask local/portico/portico
brew install --formula local/portico/portico && brew test local/portico/portico
# Arch (utilisateur non root, dans le dossier du paquet) :
makepkg --printsrcinfo | diff - .SRCINFO
namcap PKGBUILD && makepkg -f && namcap portico-bin-*.pkg.tar.zst
```

**Vérifié le 2026-10-22** (Linux x86_64, avec de fausses sommes puis de vrais binaires release locaux) :
- **Homebrew 7.0.8** (Linuxbrew, tap local) : `brew style` sans offense (formule et cask) ;
  `brew audit --strict --new --os all --arch all` de la formule et `brew audit --strict` du cask : seules
  erreurs restantes, les URL en 404 et l'API GitHub (aucune Release `v0.1.0` publiée) ; `brew install` +
  `brew test` de la formule depuis un tarball local construit comme ceux de la Release (`portico 0.1.0`,
  licences et README installés dans le Cellar).
- **Arch** (chroot depuis l'image *bootstrap* officielle, pacman à jour) : `.SRCINFO` de `render.sh`
  **identique** à `makepkg --printsrcinfo` pour les deux paquets ; `makepkg` puis `pacman -U` des deux
  (TUI depuis un tarball de Release reconstitué, GUI depuis un `.deb` au format du bundler Tauri) ;
  `portico --version` installé ; `ldd` sans bibliothèque manquante avec les seules dépendances déclarées.
  `namcap` : seulement deux avertissements attendus, `Reference to x86_64 should be changed to $CARCH`
  (les URL par architecture des paquets `-bin`) et `Unused shared library ld-linux-x86-64.so.2`
  (tout binaire Rust).
- **Workflow** : `actionlint` 1.7.12 (avec shellcheck) et `zizmor` 1.30.1 (`--offline`, aussi en
  `--persona pedantic`) sans nouvelle remarque ; contrôle des artefacts et rendu exécutés localement sur
  une fausse Release (artefact modifié refusé).

**Vérifié le 2026-10-24** (Scoop et winget, Linux x86_64, fausses sommes) : les deux `render.sh` rendent
un JSON valide et six manifestes YAML valides, refusent une version `vX.Y.Z`, un zip absent de
`SHA256SUMS` et un identifiant winget pointé, sans rien écrire ; `shellcheck` sans remarque sur les deux
scripts et sur les blocs `run:` bash du workflow. `actionlint` n'a pas pu être lancé dans cet
environnement : relecture et analyse YAML seulement.

**Non vérifié** : une exécution réelle des jobs (aucune Release publiée, ni tap, ni compte AUR, ni secret) ;
les jobs Windows (`tui` msvc, `gui-windows`, CI `gui-windows`), qui ne tournent que sur les runners
`windows-latest` ; `scoop install` et `winget validate`/`install` des fichiers rendus (à faire sur le PC
Windows du propriétaire, voir `docs/TESTING.md`) ;
le cask sur macOS (installation, `zap`, Gatekeeper) ; les paquets `aarch64` (Arch Linux ARM n'est pas un
portage officiel d'Arch : sommes rendues, paquets non construits) ; `brew audit --new` en ligne, qui
signalera probablement un dépôt GitHub encore peu « notable » (sans effet pour un tap tiers).

## Limites connues

- **Pas de mise à jour automatique** : le *updater* Tauri n'est pas activé (il exigerait une clé de
  signature des mises à jour et un point de publication ; à décider par ADR, sans service tiers imposé).
- Pas de `.rpm`, pas de Flatpak/Snap. Homebrew et l'AUR : voir les limites de « Distribution ».
- **Windows** : x64 seulement (pas d'`aarch64-pc-windows-msvc` pour l'instant), **non signé**
  (SmartScreen ; Smart App Control bloque), pas de MSI. La GUI a besoin de WebView2 Evergreen (présent
  sous Windows 11 ; installé par l'installeur sinon, ce qui demande un accès Internet à ce moment-là).
  Pageant (PuTTY) n'est pas un agent pris en charge : OpenSSH pour Windows seulement.
- **Linux non signé** (voir plus haut). L'AppImage n'embarque pas de signature GPG.
- **AppImage** : construite sur Ubuntu 22.04 (glibc 2.35) ; elle embarque WebKitGTK et GTK, d'où sa taille.
  Plus ancien que glibc 2.35 : non supporté.
- **TUI `musl`** : pas de NSS (voir plus haut) ; allocateur de musl plus lent que celui de la glibc (sans effet
  mesurable pour un client SSH interactif).
- **Builds aarch64 Linux** (runners `ubuntu-22.04-arm`) et **dmg** : configurés mais vérifiés seulement en CI,
  pas dans l'environnement de développement (Linux x86_64).
- **Outillage AppImage** : épinglé et vérifié par SHA-256 (voir « Chaîne d'approvisionnement ») ;
  les outils restent des binaires tiers de projets distincts (Tauri, linuxdeploy, AppImage), dont on
  fige la version sans pouvoir les reconstruire. Lancée avec `APPIMAGE_EXTRACT_AND_RUN=1`, l'AppImage
  journalise un avertissement Tauri (`APPDIR` détecté) sans effet.
- **TUI** : `--version` et `--help` répondent sans terminal (le `test do` de la formule Homebrew s'en sert),
  mais l'interface elle-même n'a pas de message propre hors terminal (lancée sans TTY, elle s'arrête sur une
  erreur d'initialisation) ; le test de fumée de la CI la lance donc sous un pty (`script`).
- Les builds ne sont pas encore **reproductibles** bit à bit (chemins et horodatages), ni accompagnés
  d'attestations de provenance (SLSA) : piste post-MVP.
