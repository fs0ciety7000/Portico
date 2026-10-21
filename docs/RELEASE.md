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
(et les fichiers `LICENSE*` dès qu'ils existent à la racine du dépôt).

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
4. Le workflow construit tout en parallèle (≈ 6 jobs TUI, 2 jobs GUI Linux, 1 job GUI macOS),
   puis le job `publish` calcule les sommes et crée une **Release brouillon** contenant tous les fichiers.
5. Relire le brouillon : notes de version, présence des 11 artefacts (2 deb, 2 AppImage, 1 dmg, 6 tarballs), de leurs `.sha256` et de `SHA256SUMS`, mention
   `-unsigned` ou `-not-notarized` dans le nom du dmg (voir plus bas). Publier à la main.

**Essai à blanc** : *Actions → Release → Run workflow* (`workflow_dispatch`) construit tous les
artefacts sans créer de Release ; ils restent attachés à l'exécution (90 jours).

**Construire localement** (Linux, `libwebkit2gtk-4.1-dev` installé) :
```bash
cd apps/portico-gui && pnpm install --frozen-lockfile
APPIMAGE_EXTRACT_AND_RUN=1 pnpm tauri build --bundles deb,appimage   # → target/release/bundle/
cargo build --release --locked -p portico-tui                        # → target/release/portico
```

## Secrets (macOS uniquement)

Aucun secret n'est dans le dépôt. Ils sont lus depuis les *secrets* GitHub du dépôt (ou d'un environnement)
et transmis au Tauri bundler par variables d'environnement, uniquement dans l'étape de build signée.

| Secret | Contenu |
|---|---|
| `APPLE_CERTIFICATE` | Certificat **Developer ID Application** + clé privée exportés en `.p12`, encodés en base64 (`base64 -i cert.p12 \| pbcopy`) |
| `APPLE_CERTIFICATE_PASSWORD` | Mot de passe du `.p12` |
| `APPLE_SIGNING_IDENTITY` | Nom de l'identité, ex. `Developer ID Application: Nom (TEAMID)` |
| `APPLE_ID` | Apple ID du compte développeur (notarisation) |
| `APPLE_PASSWORD` | Mot de passe **spécifique à l'app** de cet Apple ID (pas le mot de passe du compte) |
| `APPLE_TEAM_ID` | Identifiant d'équipe (10 caractères) |

Le job `gui macos` choisit son mode selon les secrets présents :

| Secrets présents | Résultat | Nom du dmg |
|---|---|---|
| les six | `.app` signée Developer ID, runtime renforcé, **notarisée et agrafée** (`stapler`) ; vérifiée par `codesign --verify --strict`, `stapler validate` et `spctl --assess` | `Portico_<v>_universal.dmg` |
| les trois de signature seulement | `.app` signée, **non notarisée** (Gatekeeper avertira) ; annotation `warning` | `Portico_<v>_universal-not-notarized.dmg` |
| aucun | signature *ad hoc* (`-`), runtime renforcé ; annotation `warning` | `Portico_<v>_universal-unsigned.dmg` |

Le mode retenu figure aussi dans le résumé du job. Sans signature, l'utilisateur doit autoriser l'app
une fois (clic droit → Ouvrir, ou *Réglages Système → Confidentialité et sécurité*).

### Ce qui est signé, et comment

- **macOS** : l'application (`Portico.app`) est signée avec le runtime renforcé, **sans App Sandbox**,
  avec un seul droit : `com.apple.security.network.client`
  ([`packaging/macos/Entitlements.plist`](../packaging/macos/Entitlements.plist)). Aucune exception du runtime
  renforcé (pas de JIT, pas de mémoire exécutable non signée, pas de variables `DYLD_*`, pas de
  désactivation de la validation des bibliothèques). Le sandbox reste désactivé parce que Portico doit lire
  `~/.ssh` et joindre le socket de `ssh-agent` ; le droit réseau est donc déclaratif tant qu'il n'y a pas de sandbox.
  L'`Info.plist` ajoute `NSLocalNetworkUsageDescription` (macOS 15 demande l'accord avant de joindre un hôte du réseau local).
  C'est l'app qui est notarisée et agrafée ; le `.dmg` qui la contient n'est pas signé séparément
  (Gatekeeper vérifie l'app agrafée, même hors ligne).
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

Les secrets restent dans le trousseau système : sous Linux, un service Secret Service (GNOME Keyring,
KeePassXC, KWallet ≥ 5.97…) doit tourner dans la session ; à défaut, Portico propose son vault chiffré.

## Limites connues

- **Pas de mise à jour automatique** : le *updater* Tauri n'est pas activé (il exigerait une clé de
  signature des mises à jour et un point de publication ; à décider par ADR, sans service tiers imposé).
- **Pas de Windows**, pas de `.rpm`, pas de Flatpak/Snap, pas de formule Homebrew pour l'instant.
- **Linux non signé** (voir plus haut). L'AppImage n'embarque pas de signature GPG.
- **AppImage** : construite sur Ubuntu 22.04 (glibc 2.35) ; elle embarque WebKitGTK et GTK, d'où sa taille.
  Plus ancien que glibc 2.35 : non supporté.
- **TUI `musl`** : pas de NSS (voir plus haut) ; allocateur de musl plus lent que celui de la glibc (sans effet
  mesurable pour un client SSH interactif).
- **Fichiers de licence** : le dépôt déclare `MIT OR Apache-2.0` mais ne contient pas encore
  `LICENSE-MIT` / `LICENSE-APACHE`. Le workflow les embarquera dans les tarballs dès qu'ils existent ;
  à ajouter avant la première release publique.
- **Builds aarch64 Linux** (runners `ubuntu-22.04-arm`) et **dmg** : configurés mais vérifiés seulement en CI,
  pas dans l'environnement de développement (Linux x86_64).
- **Outillage AppImage non épinglé** : le Tauri bundler télécharge `linuxdeploy` (version figée par Tauri) et
  `linuxdeploy-plugin-appimage` (canal `continuous`) depuis GitHub au moment du build. Lancée avec
  `APPIMAGE_EXTRACT_AND_RUN=1`, l'AppImage journalise un avertissement Tauri (`APPDIR` détecté) sans effet.
- **TUI** : pas encore d'option `--version` ni de message propre hors terminal (lancée sans TTY, elle
  s'arrête sur une erreur d'initialisation) ; le test de fumée de la CI la lance donc sous un pty (`script`).
- Les builds ne sont pas encore **reproductibles** bit à bit (chemins et horodatages), ni accompagnés
  d'attestations de provenance (SLSA) : piste post-MVP.
