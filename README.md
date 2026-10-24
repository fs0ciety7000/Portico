<p align="center">
  <img src="design/portico-mark.svg" width="96" alt="Portico" />
</p>

<h1 align="center">Portico</h1>

<p align="center">
  Gestionnaire de sessions SSH <b>local-first</b>, rapide et <b>keyboard-first</b>, pour macOS et Linux.<br/>
  Un moteur Rust, deux interfaces : une TUI et une GUI.
</p>

---

- **Zéro lock-in** : tout fonctionne hors ligne. Vos données Portico sont en TOML lisible.
- **Votre setup est la source de vérité** : `~/.ssh/config` (Include, Match, ProxyJump…), `known_hosts`, `ssh-agent` et vos clés.
- **Sécurisé** : secrets dans le trousseau système (Keychain, Secret Service), jamais en clair.
- **Premium** : trois thèmes sombres (Obsidian, Basalt, Travertine Night) et un clair (Marble), plus **Auto** qui suit le système (Marble le jour, votre thème sombre la nuit) ; statut de connexion et latence toujours visibles.

> 🚧 **Phase 0 terminée** (fondations). La connexion interactive arrive en phase 1, voir [`docs/PLAN.md`](docs/PLAN.md).

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
