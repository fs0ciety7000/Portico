# Raccourcis clavier : Portico

> Source de vérité : `apps/portico-tui/src/keymap.rs` (TUI) et la palette de `apps/portico-gui/src/App.svelte` (GUI).
> Toute modification de raccourci met à jour ce fichier. `⌘` désigne `Cmd` sur macOS et `Ctrl` sur Linux.

## Disponibles (phase 0)

| Action | TUI | GUI |
|---|---|---|
| Descendre / monter dans la liste | `j` / `k`, `↓` / `↑` | `j` / `k`, `↓` / `↑` |
| Premier / dernier hôte | `g` / `G` | — |
| Rechercher (flou, `#tag`, `*` favoris) | `/` ou `Ctrl-K` | `/` |
| Naviguer pendant la recherche | `↑` `↓`, `Ctrl-N` / `Ctrl-P` | `↑` `↓`, `Ctrl-N` / `Ctrl-P` |
| Vider la recherche | `Ctrl-U` (ou `Esc` hors recherche) | `Esc` |
| Se connecter | `↵` | `↵`, double-clic |
| Se déconnecter (hôte sélectionné) | `d` | `d` |
| Palette de commandes | — *(phase 1b)* | `⌘K` |
| Recharger `~/.ssh/config` | `r` | `⌘R` |
| Changer de thème | `t` | `t` |
| Aide | `?` | (palette) |
| Fermer overlay / quitter la recherche | `Esc` | `Esc` |
| Quitter | `q`, `Ctrl-C` | `⌘Q` (système) |

## Prévus

| Action | TUI | GUI | Phase |
|---|---|---|---|
| Préfixe de commande dans un terminal | `Ctrl-Space` | — | 1b |
| Nouvel onglet / fermer | `Ctrl-Space n` / `Ctrl-Space w` | `⌘T` / `⌘W` | 1b / 1c |
| Aller à l'onglet n | `Ctrl-Space 1..9` | `⌘1..9` | 1b / 1c |
| Snippets | `Ctrl-Space s` | `⌘K` puis « Snippet » | 3 |
| Explorateur SFTP | `Ctrl-Space f` | `⌘⇧F` | 2 |
| Tunnels | `Ctrl-Space p` | `⌘⇧P` | 2 |
| Logs / debug de la session | `Ctrl-Space l` | `⌘⇧L` | 4 |
| Favori on/off | `f` | `⌘D` | 4 |
