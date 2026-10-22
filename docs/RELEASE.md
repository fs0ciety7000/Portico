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
| `portico-<v>-<target>.tar.gz` (TUI) | `x86_64`/`aarch64-unknown-linux-gnu`, `x86_64`/`aarch64-unknown-linux-musl`, `x86_64`/`aarch64-apple-darwin` | `cargo build --release` | non (somme SHA256) |
| `SHA256SUMS` et un `<fichier>.sha256` par artefact | toutes | job `publish` | — |

Chaque tarball TUI contient un dossier `portico-<v>-<target>/` avec le binaire `portico` et `README.md`
(et les fichiers `LICENSE-MIT` et `LICENSE-APACHE` de la racine du dépôt).

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
4. Le workflow construit tout en parallèle (6 jobs TUI, 2 jobs GUI Linux, 1 job `gui-macos-build`
   sans secret), puis `gui-macos-sign` attend l'**approbation d'un reviewer** de l'environnement
   `release`, signe et notarise l'app ; enfin le job `publish` calcule les sommes et crée une
   **Release brouillon** contenant tous les fichiers.
5. Relire le brouillon : notes de version, présence des 11 artefacts (2 deb, 2 AppImage, 1 dmg, 6 tarballs), de leurs `.sha256` et de `SHA256SUMS`, mention
   `-unsigned` ou `-not-notarized` dans le nom du dmg (voir plus bas). Publier à la main
   (une Release ni brouillon ni *pre-release*).
6. La publication déclenche la **distribution** (tap Homebrew et AUR, voir « Distribution ») :
   fusionner la PR ouverte sur le tap, vérifier les deux paquets sur l'AUR.

**Essai à blanc** : *Actions → Release → Run workflow* (`workflow_dispatch`, champ `distribute`
vide) construit tous les artefacts sans créer de Release et **sans signer** (le job de signature ne
tourne que sur un push de tag) ; ils restent attachés à l'exécution (90 jours).

**Construire localement** (Linux, `libwebkit2gtk-4.1-dev` installé) :
```bash
cd apps/portico-gui && pnpm install --frozen-lockfile
APPIMAGE_EXTRACT_AND_RUN=1 pnpm tauri build --bundles deb,appimage   # → target/release/bundle/
cargo build --release --locked -p portico-tui                        # → target/release/portico
```

## Secrets (macOS uniquement)

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
une fois (clic droit → Ouvrir, ou *Réglages Système → Confidentialité et sécurité*).

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

Les secrets restent dans le trousseau système : sous Linux, un service Secret Service (GNOME Keyring,
KeePassXC, KWallet ≥ 5.97…) doit tourner dans la session ; à défaut, Portico propose son vault chiffré.

## Distribution (Homebrew, AUR)

Les paquets sont **générés depuis la Release publiée** : mêmes fichiers, mêmes sommes que `SHA256SUMS`,
rien n'est reconstruit.

| Cible | Paquet | Source | Gabarit et rendu |
|---|---|---|---|
| Tap `<owner>/homebrew-portico` | formule `portico` (TUI) | tarballs `aarch64`/`x86_64-apple-darwin` et `aarch64`/`x86_64-unknown-linux-gnu` (`on_macos`/`on_linux`, `on_arm`/`on_intel`) | [`packaging/homebrew/`](../packaging/homebrew/) |
| Tap `<owner>/homebrew-portico` | cask `portico` (GUI) | `Portico_<v>_universal*.dmg`, `depends_on macos: :ventura` (= 13 ou plus, comme `minimumSystemVersion`) | idem |
| AUR | `portico-bin` (TUI) | tarballs `x86_64`/`aarch64-unknown-linux-gnu` | [`packaging/aur/`](../packaging/aur/) |
| AUR | `portico-gui-bin` (GUI) | `.deb` `amd64`/`arm64`, extrait | idem |

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
   *pre-release* est ignorée) lance trois jobs, et **aucun build** :
   - `dist-render` (aucun secret, `contents: read`) : vérifie que la Release est publiée et finale
     (`vX.Y.Z`, ni brouillon ni *pre-release*), télécharge les 7 artefacts utiles (4 tarballs, 2 `.deb`,
     le dmg) et `SHA256SUMS`, les vérifie (`sha256sum --strict -c`), rend les fichiers
     (`render.sh` refuse un artefact absent de `SHA256SUMS` ou une somme mal formée), contrôle la
     syntaxe (`ruby -c`, `bash -n`) et les transmet dans l'artefact `distribution` (7 jours) ;
   - `homebrew` (`permissions: {}`, secret `HOMEBREW_TAP_TOKEN`) : pousse `Formula/portico.rb` et
     `Casks/portico.rb` sur la branche `portico-<v>` du tap et **ouvre une PR** (ou met à jour celle qui
     existe). La PR laisse au propriétaire le dernier mot, et à la CI du tap (`brew test-bot`) le temps
     de tourner ;
   - `aur` (`permissions: {}`, secret `AUR_SSH_PRIVATE_KEY`) : pousse `PKGBUILD` et `.SRCINFO` sur
     `master` de `portico-bin` et `portico-gui-bin` (commit `Update to <v>-<pkgrel>`). L'AUR n'a pas de
     PR. La clé d'hôte de `aur.archlinux.org` n'est acceptée que si son empreinte ed25519 est celle
     publiée sur la page d'accueil de l'AUR (`SHA256:RFzBCUItH9LZS0cKB5UE6ceAYhBD5C8GeOBip8Z11+4`).
2. Les deux jobs à secret n'exécutent **aucun code du dépôt** ni action tierce (sauf
   `download-artifact`) : `git`, `gh` et `ssh` du runner, sur les fichiers rendus. Le jeton du tap passe
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

### Vérifications

```bash
# Rendu depuis le SHA256SUMS d'une Release (gh release download vX.Y.Z -p SHA256SUMS) :
packaging/homebrew/render.sh X.Y.Z SHA256SUMS out/homebrew
packaging/aur/render.sh X.Y.Z SHA256SUMS out/aur
ruby -c out/homebrew/Formula/portico.rb out/homebrew/Casks/portico.rb
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

**Non vérifié** : une exécution réelle des jobs (aucune Release publiée, ni tap, ni compte AUR, ni secret) ;
le cask sur macOS (installation, `zap`, Gatekeeper) ; les paquets `aarch64` (Arch Linux ARM n'est pas un
portage officiel d'Arch : sommes rendues, paquets non construits) ; `brew audit --new` en ligne, qui
signalera probablement un dépôt GitHub encore peu « notable » (sans effet pour un tap tiers).

## Limites connues

- **Pas de mise à jour automatique** : le *updater* Tauri n'est pas activé (il exigerait une clé de
  signature des mises à jour et un point de publication ; à décider par ADR, sans service tiers imposé).
- **Pas de Windows**, pas de `.rpm`, pas de Flatpak/Snap. Homebrew et l'AUR : voir les limites de « Distribution ».
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
