<p align="center">
  <img src="design/portico-mark.svg" width="96" alt="Portico" />
</p>

<h1 align="center">Portico</h1>

<p align="center">
  Gestionnaire de sessions SSH <b>local-first</b>, rapide et <b>keyboard-first</b>, pour macOS, Linux et Windows.<br/>
  Un moteur Rust, deux interfaces : une TUI et une GUI.
</p>

---

- **Zéro lock-in** : tout fonctionne hors ligne. Vos données Portico sont en TOML lisible.
- **Votre setup est la source de vérité** : `~/.ssh/config` (Include, Match, ProxyJump…), `known_hosts`, `ssh-agent` et vos clés.
- **Synchronisation chiffrée, sans cloud de Portico** : hôtes, tags, snippets et réglages passent d'un appareil à l'autre par un dossier que vous synchronisez déjà (iCloud Drive, Dropbox, OneDrive, Syncthing, git…) ; seuls des fichiers chiffrés en sortent, jamais vos clés privées ni `known_hosts`, et chaque hôte ou snippet reçu attend votre confirmation.
- **Sécurisé** : secrets dans le trousseau système (Keychain, Secret Service, Gestionnaire d'identification Windows), jamais en clair.
- **Premium** : trois thèmes sombres (Obsidian, Basalt, Travertine Night) et un clair (Marble), plus **Auto** qui suit le système (Marble le jour, votre thème sombre la nuit) ; statut de connexion et latence toujours visibles.

> 🚧 **Phase 0 terminée** (fondations). La connexion interactive arrive en phase 1, voir [`docs/PLAN.md`](docs/PLAN.md).

## Installer

Chaque [Release](https://github.com/fs0ciety7000/Portico/releases) publie les fichiers et leur `SHA256SUMS`
(vérification, signature et détails : [`docs/RELEASE.md`](docs/RELEASE.md)).

| Système | GUI | TUI (`portico`) |
|---|---|---|
| macOS 13+ | `brew install --cask fs0ciety7000/portico/portico`, ou le `.dmg` universel | `brew install fs0ciety7000/portico/portico` |
| Linux | `.deb` (Debian 12+, Ubuntu 22.04+), AppImage, AUR `portico-gui-bin` | tarball `gnu` ou `musl`, Homebrew, AUR `portico-bin` |
| Windows 11 x64 | `Portico_<v>_x64-setup.exe` (par utilisateur, sans droits admin), `winget install fs0ciety7000.Portico` | zip, `scoop bucket add portico https://github.com/fs0ciety7000/scoop-portico` puis `scoop install portico/portico`, `winget install fs0ciety7000.PorticoTUI` |

Sous Windows, les binaires ne sont **pas encore signés** : au premier lancement de l'installeur,
SmartScreen demande *Informations complémentaires → Exécuter quand même* (*More info → Run anyway*),
après vérification de la somme. Portico y lit `%USERPROFILE%\.ssh` et l'agent d'OpenSSH pour Windows.

## Démarrer

```bash
# TUI
cargo run -p portico-tui            # binaire `portico`

# GUI (Linux : libwebkit2gtk-4.1-dev requis)
cd apps/portico-gui && pnpm install && pnpm tauri dev

# Itérer sur le design de la GUI dans un navigateur (données de démo)
cd apps/portico-gui && pnpm dev
```

## Développement

```bash
cargo fmt --all && cargo clippy --all-targets && cargo test
cargo run -p portico-theme --bin export-tokens   # après modification des tokens de design
```

| Document | Contenu |
|---|---|
| [`CLAUDE.md`](CLAUDE.md) | Contrat du projet : principes, architecture, conventions |
| [`docs/STACK.md`](docs/STACK.md) | Stack retenue, alternatives, journal des décisions |
| [`docs/PLAN.md`](docs/PLAN.md) | Plan MVP par phases et roadmap |
| [`docs/KEYBINDINGS.md`](docs/KEYBINDINGS.md) | Raccourcis clavier |
| [`docs/IMPORT.md`](docs/IMPORT.md) | Importer ses hôtes depuis Termius ou Termix (`portico import FILE`, clés sur sélection, palette de la GUI) |

## Licence

Au choix, sous licence [MIT](LICENSE-MIT) ou [Apache 2.0](LICENSE-APACHE) (`MIT OR Apache-2.0`).
Sauf mention contraire, toute contribution soumise à ce dépôt est proposée sous ces deux licences, sans
conditions supplémentaires.
