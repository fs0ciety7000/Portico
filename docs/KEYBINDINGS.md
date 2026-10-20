# Raccourcis clavier : Portico

> Source de vérité : `apps/portico-tui/src/keymap.rs` (TUI, aide `?` générée depuis `HELP`) et la palette
> de `apps/portico-gui/src/App.svelte` (GUI). Toute modification de raccourci met à jour ce fichier.
> `⌘` désigne `Cmd` sur macOS. Sur Linux, le modificateur de l'app est `Ctrl` dans la liste des hôtes et
> **`Ctrl-Shift`** dès qu'un terminal a le focus (`Ctrl-T`, `Ctrl-W`, `Ctrl-K`… appartiennent au shell, à vim, à tmux),
> comme GNOME Terminal. `Ctrl-Shift` fonctionne partout, c'est ce que l'UI affiche. Les onglets numérotés passent par
> **`Alt-1`…`Alt-9`** (`Ctrl-Shift-2` et `-6` sont `Ctrl-@` et `Ctrl-^`, utilisés par emacs et vim).
> Les lettres sont reconnues par le caractère produit (AZERTY, Dvorak, Bépo) ; les chiffres par la touche physique.

## Liste des hôtes

| Action | TUI | GUI |
|---|---|---|
| Descendre / monter | `j` / `k`, `↓` / `↑` | `j` / `k`, `↓` / `↑` |
| Premier / dernier hôte | `g` / `G` | — |
| Rechercher (flou, `#tag`, `*` favoris) | `/` ou `Ctrl-K` | `/` |
| Naviguer pendant la recherche | `↑` `↓`, `Ctrl-N` / `Ctrl-P` | `↑` `↓`, `Ctrl-N` / `Ctrl-P` |
| Vider la recherche | `Ctrl-U` (ou `Esc` hors recherche) | `Esc` |
| Se connecter (ouvre un onglet terminal, ou y revient) | `↵` | `↵`, double-clic |
| Se déconnecter (hôte sélectionné) | `d` | `d` |
| Aller à l'onglet n / suivant / précédent | `1`-`9`, `]` / `Tab`, `[` / `Shift-Tab` | `⌘1`…`⌘9` (Linux : `Alt-1`…`9`), `Ctrl-Tab` / `Ctrl-Shift-Tab` |
| Préfixe des commandes d'onglet (comme dans un terminal) | `Ctrl-Space` | — |
| Palette de commandes | — | `⌘K` |
| Recharger `~/.ssh/config` | `r` | `⌘R` (Linux : `Ctrl-R`) |
| Changer de thème | `t` | `t` |
| Aide | `?` | (palette) |
| Quitter | `q`, `Ctrl-C` | `⌘Q` (système) |

## Terminal (TUI)

Toutes les touches partent vers l'hôte distant, y compris `Ctrl-C`. Les commandes Portico passent par le
préfixe **`Ctrl-Space`** (la barre de statut affiche `PREFIX`). Le préfixe fonctionne aussi depuis la liste
des hôtes, avec la même table : une seule grammaire à retenir. Il s'applique à une seule touche, et `Esc`
l'annule.

| Action | Touches |
|---|---|
| Retour à la liste des hôtes | `Ctrl-Space` puis `h` (ou `0`) |
| Onglet n / suivant / précédent | `Ctrl-Space` puis `1`-`9`, `]` / `[` |
| Nouvel onglet sur le même hôte | `Ctrl-Space` puis `n` |
| Fermer l'onglet (le dernier onglet d'un hôte le déconnecte) | `Ctrl-Space` puis `w` |
| Déconnecter cet hôte | `Ctrl-Space` puis `d` |
| Rechercher un hôte | `Ctrl-Space` puis `/` |
| Aide | `Ctrl-Space` puis `?` |
| Envoyer `Ctrl-Space` au programme distant | `Ctrl-Space` deux fois |
| Remonter / descendre dans l'historique | `Shift-PgUp` / `Shift-PgDn` (toute frappe revient en bas) |
| Coller | collage du terminal hôte (bracketed paste si le programme distant l'active) |

## Dialogues

| Dialogue | Touches |
|---|---|
| Nouvelle clé d'hôte | `y` faire confiance et enregistrer · `n` / `Esc` refuser |
| Clé d'hôte inhabituelle (autre type déjà connu) | taper `yes` puis `↵` · `Esc` refuser |
| Mot de passe / passphrase | saisie masquée · `Tab` champ suivant / case « mémoriser » · `Espace` cocher · `↵` valider · `Esc` / `Ctrl-C` annuler |
| Keyboard-interactive | `Tab` entre les champs · `↵` valider · `Esc` annuler |
| Clé d'hôte modifiée (alerte) | `Esc` / `↵` fermer (aucune acceptation possible) |

## Terminal (GUI)

Toutes les touches partent vers l'hôte distant, sauf les raccourcis de l'app ci-dessous.

| Action | macOS | Linux |
|---|---|---|
| Nouvel onglet sur le même hôte (depuis la liste : se connecter) | `⌘T` | `Ctrl-Shift-T` |
| Fermer l'onglet (le dernier onglet d'un hôte le déconnecte ; pendant la connexion : annuler) | `⌘W` | `Ctrl-Shift-W` |
| Onglet n / liste des hôtes | `⌘1`…`⌘9` / `⌘0` | `Alt-1`…`Alt-9` / `Alt-0` |
| Onglet suivant / précédent | `Ctrl-Tab` / `Ctrl-Shift-Tab`, `⌘]` / `⌘[` | `Ctrl-Tab` / `Ctrl-Shift-Tab` |
| Palette de commandes | `⌘K` | `Ctrl-Shift-K` |
| Copier la sélection / coller | `⌘C` / `⌘V` | `Ctrl-Shift-C` / `Ctrl-Shift-V`, `Shift-Insert`, clic milieu |
| Reconnecter un onglet terminé ou en échec | `↵` | `↵` |
| Annuler une connexion en cours / revenir aux hôtes (onglet sans shell) | `Esc` | `Esc` |
| Barre d'onglets (focus clavier) : onglet voisin, premier / dernier, fermer | `←` `→`, `Home` / `End`, `Suppr` | idem |
| Fermer un onglet (souris) | clic milieu sur l'onglet | clic milieu sur l'onglet |

Tout collage est assaini (ESC et contrôles C1 retirés) avant d'atteindre le terminal.

## Prévus

| Action | TUI | GUI | Phase |
|---|---|---|---|
| Snippets | `Ctrl-Space s` | `⌘K` puis « Snippet » | 3 |
| Explorateur SFTP | `Ctrl-Space f` | `⌘⇧F` | 2 |
| Tunnels | `Ctrl-Space p` | `⌘⇧P` | 2 |
| Logs / debug de la session | `Ctrl-Space l` | `⌘⇧L` | 4 |
| Favori on/off | `f` | `⌘D` | 4 |
