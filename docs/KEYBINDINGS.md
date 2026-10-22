# Raccourcis clavier : Portico

> Sources de vérité : `apps/portico-tui/src/keymap.rs` pour la TUI (aide `?`, barre de statut, tables du bloc
> `BEGIN/END GENERATED: tui`, régénérées par `cargo run -p portico-tui -- --write-keybindings docs/KEYBINDINGS.md`)
> et `apps/portico-gui/src/lib/shortcuts.ts` pour la GUI (palette, infobulles, bloc `BEGIN/END GENERATED: gui`,
> régénéré par `pnpm gen:keybindings`). Un test échoue de chaque côté si son bloc est périmé : ne modifiez pas
> ces blocs à la main. Les raccourcis se personnalisent dans `settings.toml` (voir les sections dédiées).
> `⌘` désigne `Cmd` sur macOS. Sur Linux, le modificateur de l'app est `Ctrl` dans la liste des hôtes et
> **`Ctrl-Shift`** dès qu'un terminal a le focus (`Ctrl-T`, `Ctrl-W`, `Ctrl-K`… appartiennent au shell, à vim, à tmux),
> comme GNOME Terminal. `Ctrl-Shift` fonctionne partout, c'est ce que l'UI affiche. Les onglets numérotés passent par
> **`Alt-1`…`Alt-9`** (`Ctrl-Shift-2` et `-6` sont `Ctrl-@` et `Ctrl-^`, utilisés par emacs et vim).
> Les lettres sont reconnues par le caractère produit (AZERTY, Dvorak, Bépo) ; les chiffres par la touche physique.

## Vue d'ensemble

Une seule grammaire dans les deux interfaces : `↵` se connecte, `/` cherche (`#tag`, `@groupe`, `*` favoris),
`f` fichiers, `p` tunnels, `i` clés, `s` snippets, `e` édition des métadonnées d'un hôte, `*` (TUI) ou `⌘D`
(GUI, liste des hôtes) favori, `l` (TUI) ou `⌘⇧L` (GUI) logs de session, `t` thème. La TUI passe par le préfixe
`Ctrl-Space` dans un terminal ; la GUI par `⌘` (Linux : `Ctrl`, `Ctrl-Shift` quand un terminal a le focus) et la
palette `⌘K`. Dans un terminal GUI, `⌘D` divise le panneau (voir « Panneaux (GUI) : principe »). Les tables
complètes et à jour sont les blocs générés ci-dessous.

## Configurer les raccourcis et le thème (`settings.toml`)

`settings.toml` vit dans le dossier de configuration de Portico (`~/.config/portico/` sous Linux,
`~/Library/Application Support/dev.Portico.portico/` sous macOS). Portico le lit au démarrage ; `t` (TUI) y enregistre le
thème choisi en gardant le reste du fichier (clés inconnues et commentaires compris).

```toml
theme = "basalt"             # obsidian (défaut), basalt, travertine

[keymap.tui]
"hosts.files" = "F"          # une touche…
"prefix.debug" = "L"
"hosts.down" = "n down"      # …ou plusieurs, séparées par des espaces (4 au plus)
"terminal.scroll_up" = "alt-pgup"
```

- **Identifiants** : colonne « Identifiant » des tables TUI ci-dessous (`hosts.*` : liste des hôtes,
  `prefix.*` : après `Ctrl-Space`, `terminal.*` : onglet terminal). Une entrée remplace toutes les touches
  par défaut de l'action.
- **Syntaxe des touches** : un caractère tel quel (`f`, `F` pour Shift-f, `*`, `?`) ; des noms sans casse
  (`enter`, `esc`, `tab`, `shift-tab`, `space`, `backspace`, `delete`, `insert`, `home`, `end`, `pgup`,
  `pgdn`, `up`, `down`, `left`, `right`, `f1`…`f12`) ; des modificateurs `ctrl-`, `alt-`, `shift-` (ou `+` :
  `ctrl+k`). `shift-` ne s'applique qu'aux touches nommées (écrire `G`, pas `shift-g`).
- **Refusées** (ignorées au démarrage, avec un toast qui les liste) : action inconnue, touche illisible, plus
  de 4 touches, touche réservée (`Ctrl-C`, `Esc`, `Ctrl-Space`, les chiffres `1`-`9` des onglets), touche
  d'un terminal sans `Ctrl`, `Alt` ni `Shift`, et conflit avec une autre action du même contexte (deux
  entrées sur la même touche sont ignorées toutes les deux ; échanger deux touches fonctionne).
- Un thème inconnu revient au thème par défaut, avec un toast. Un fichier invalide est ignoré en entier
  (toast avec l'erreur) ; il n'est jamais réécrit tant qu'il ne se relit pas.
- L'aide `?` et la barre de statut affichent toujours les touches effectives.
- Ligne de commande : `portico --help`, `portico --version`, `portico --print-keybindings` (tables TUI en
  Markdown). Sans terminal (stdin ou stdout redirigé), Portico le dit et sort avec le code 1.

<!-- BEGIN GENERATED: tui -->
<!-- Généré depuis apps/portico-tui/src/keymap.rs et keydoc.rs : ne pas éditer à la main.
     cargo run -p portico-tui -- --write-keybindings docs/KEYBINDINGS.md -->

### TUI : liste des hôtes

Sans recherche, la liste est organisée en sections : « Recent » (5 derniers hôtes encore présents dans la config), puis les groupes (ordre alphabétique), puis « Other » ; sans groupe ni récent, elle reste plate. Les touches de cette table sont configurables (voir « Configurer les raccourcis »).

| Action | Touches | Identifiant |
|---|---|---|
| Descendre | `j`, `↓` | `hosts.down` |
| Monter | `k`, `↑` | `hosts.up` |
| Premier hôte | `g`, `Home` | `hosts.first` |
| Dernier hôte | `G`, `End` | `hosts.last` |
| Rechercher (flou ; `#tag`, `*` favoris, `@groupe`) | `/`, `Ctrl-K` | `hosts.search` |
| Vider la recherche (aussi `Esc`) | `Ctrl-U` | `hosts.clear_search` |
| Se connecter (ouvre un onglet terminal, ou y revient) | `↵` | `hosts.connect` |
| Se déconnecter (hôte sélectionné) | `d` | `hosts.disconnect` |
| Explorateur de fichiers (SFTP) de l'hôte (se connecte au besoin) | `f` | `hosts.files` |
| Tunnels | `p` | `hosts.tunnels` |
| Clés (locales, agent, vault) | `i` | `hosts.keys` |
| Snippets de l'hôte | `s` | `hosts.snippets` |
| « Add host… » : créer un hôte Portico (dans `hosts.conf`, jamais dans `~/.ssh/config`) | `a`, `n` | `hosts.add` |
| « Edit host… » : modifier un hôte Portico ; un hôte de `~/.ssh/config` (ou importé) reste en lecture seule : la vue propose de le dupliquer (`D`) ou d'éditer ses tags, groupe et notes (`t`, `hosts.toml`) | `e` | `hosts.edit` |
| « Duplicate as Portico host… » : copie modifiable de l'hôte sélectionné, sous un nouvel alias | `D` | `hosts.duplicate` |
| « Delete host » : supprimer un hôte Portico, ses métadonnées et son mot de passe mémorisé (confirmation armée : seul `y` supprime) | `Del` | `hosts.delete` |
| Copier (OSC 52) la ligne `Include` à ajouter en tête de `~/.ssh/config`, quand le bandeau est affiché | `y` | `hosts.copy_include` |
| Masquer le bandeau `Include` jusqu'au prochain lancement | `z` | `hosts.hide_include` |
| Favori on/off | `*` | `hosts.favorite` |
| Journaux de session (panneau debug) et config effective | `l` | `hosts.debug` |
| « Recordings » : enregistrements de sessions (relire, supprimer) | `v` | `hosts.recordings` |
| Retirer l'hôte des récents (ligne de la section Recent) | `x` | `hosts.forget_recent` |
| Vider les récents (confirmation `y`) | `X` | `hosts.clear_recent` |
| Onglet suivant | `]`, `Tab` | `hosts.next_tab` |
| Onglet précédent | `[`, `Shift-Tab` | `hosts.prev_tab` |
| Recharger `~/.ssh/config` | `r` | `hosts.reload` |
| Changer de thème (enregistré dans `settings.toml`) | `t` | `hosts.theme` |
| Aide | `?` | `hosts.help` |
| Quitter (aussi `Ctrl-C`) | `q` | `hosts.quit` |
| Aller à l'onglet n | `1`-`9` | (fixe) |
| Vider la recherche | `Esc` | (fixe) |
| Quitter | `Ctrl-C` | (fixe) |
| Aide : page suivante, quand elle ne tient pas à l'écran (80×24 : hôtes, préfixe, journaux et édition, puis fichiers et panneaux) | `Tab`, `←` / `→` (aide ouverte) | (fixe) |
| Préfixe des commandes d'onglet (comme dans un terminal) | `Ctrl-Space` | (fixe) |

### TUI : recherche

Le champ de recherche (`/`) prend la saisie : ses touches sont fixes. `#tag`, `*` (favoris) et `@groupe` filtrent ; la liste devient plate et classée.

| Action | Touches |
|---|---|
| Naviguer pendant la recherche | `↑` `↓`, `Ctrl-N` / `Ctrl-P` |
| Se connecter à l'hôte sélectionné | `↵` |
| Effacer un caractère / tout | `Backspace` / `Ctrl-U` |
| Quitter le champ (le filtre reste) | `Esc` |

### TUI : préfixe `Ctrl-Space`

Dans un terminal, toutes les touches partent vers l'hôte distant, y compris `Ctrl-C`. Les commandes Portico passent par le préfixe **`Ctrl-Space`** (la barre de statut affiche `PREFIX`). Le préfixe fonctionne aussi depuis la liste des hôtes et les panneaux, avec la même table. Il s'applique à une seule touche.

| Action | Touches | Identifiant |
|---|---|---|
| Retour à la liste des hôtes | `h`, `0` | `prefix.home` |
| Onglet suivant | `]`, `Tab` | `prefix.next_tab` |
| Onglet précédent | `[`, `Shift-Tab` | `prefix.prev_tab` |
| Nouvel onglet sur le même hôte | `n` | `prefix.new_tab` |
| Fermer l'onglet et tous ses panneaux (un hôte qui n'a plus aucun panneau est déconnecté) | `w` | `prefix.close_tab` |
| « Split down » : nouveau panneau dessous, même hôte (même session si elle est vivante) | `"`, `-` | `prefix.split_horizontal` |
| « Split right » : nouveau panneau à droite, même hôte | `%`, `\|` | `prefix.split_vertical` |
| « Split with host… » : diviser vers un autre hôte (choisi dans la liste ; sa session vivante est réutilisée, sinon une connexion s'ouvre) | `S` | `prefix.split_host` |
| Focus au panneau de gauche (puis les flèches seules pendant 600 ms : mode `PANE`) | `←` | `prefix.pane_left` |
| Focus au panneau du dessous (mode `PANE`) | `↓` | `prefix.pane_down` |
| Focus au panneau du dessus (mode `PANE`) | `↑` | `prefix.pane_up` |
| Focus au panneau de droite (mode `PANE`) | `→` | `prefix.pane_right` |
| « Next pane » : panneau suivant (ordre de lecture, boucle) | `o` | `prefix.next_pane` |
| « Move divider » : séparation verticale vers la gauche, 4 colonnes (puis `Shift` + flèche seul pendant 600 ms : mode `PANE`) | `Shift-←` | `prefix.resize_left` |
| « Move divider » : séparation horizontale vers le bas, 2 lignes (mode `PANE`) | `Shift-↓` | `prefix.resize_down` |
| « Move divider » : séparation horizontale vers le haut, 2 lignes (mode `PANE`) | `Shift-↑` | `prefix.resize_up` |
| « Move divider » : séparation verticale vers la droite, 4 colonnes (mode `PANE`) | `Shift-→` | `prefix.resize_right` |
| « Close pane » : fermer le panneau (son canal PTY ; le dernier panneau ferme l'onglet, le dernier panneau d'une session la déconnecte) | `x` | `prefix.close_pane` |
| « Zoom pane » : le panneau seul dans l'onglet, ou retour à la division | `z` | `prefix.zoom_pane` |
| Déconnecter cet hôte | `d` | `prefix.disconnect` |
| Explorateur de fichiers (SFTP) | `f` | `prefix.files` |
| Tunnels | `p` | `prefix.tunnels` |
| Clés (locales, agent, vault) | `i` | `prefix.keys` |
| Snippets de l'hôte de l'onglet | `s` | `prefix.snippets` |
| Panneau debug (présélectionne la session de l'onglet) | `l` | `prefix.debug` |
| « Record session » : enregistrer le panneau actif (asciicast v2 ; confirmation, seul `y` démarre) ; s'il est déjà enregistré, « Stop recording » l'arrête sans confirmation | `R` | `prefix.record` |
| « Recordings » : enregistrements de sessions (relire, supprimer) | `v` | `prefix.recordings` |
| Rechercher un hôte | `/` | `prefix.search` |
| Aide | `?` | `prefix.help` |
| Onglet n | `1`-`9` | (fixe) |
| Envoyer `Ctrl-Space` au programme distant | `Ctrl-Space` deux fois | (fixe) |
| Annuler le préfixe | `Esc` (ou toute touche non liée) | (fixe) |

### TUI : terminal

Les touches configurables d'un terminal doivent porter `Ctrl`, `Alt` ou `Shift` : les autres appartiennent au programme distant.

| Action | Touches | Identifiant |
|---|---|---|
| Remonter dans l'historique | `Shift-PgUp` | `terminal.scroll_up` |
| Redescendre dans l'historique (toute frappe revient en bas) | `Shift-PgDn` | `terminal.scroll_down` |
| Coller | collage du terminal hôte (bracketed paste si le programme distant l'active) | (fixe) |

### TUI : panneaux

Un onglet terminal se divise en panneaux : 8 au plus, chacun d'au moins 20×5 cellules. Les commandes sont dans la table du préfixe (`prefix.split_*`, `prefix.pane_*`, `prefix.next_pane`, `prefix.resize_*`, `prefix.close_pane`, `prefix.zoom_pane`). Seul le panneau actif reçoit les frappes, le collage et le défilement (aucune diffusion).

| Action | Touches |
|---|---|
| Choisir l'hôte du nouveau panneau (après `Ctrl-Space S` : liste « Split with host… », recherche vide et active, rendue ensuite) | saisie pour filtrer, `↓` / `↑`, `↵` pour diviser |
| Annuler le choix d'hôte | `Esc`, `Ctrl-C` |
| Mode `PANE` : après `Ctrl-Space` + flèche ou `Shift` + flèche, les flèches (focus) et `Shift` + flèches (« Move divider ») restent actives 600 ms sans le préfixe ; toute autre touche quitte le mode et part au panneau | `←` `↓` `↑` `→`, `Shift` + flèche |
| Rouvrir un panneau dont la connexion a échoué ou s'est fermée (reconnecte au besoin ; jamais après un changement de clé hôte) | `↵` (dans le panneau actif ; un autre panneau en échec indique « focus to retry (Ctrl-Space ←↓↑→) ») |

Chaque panneau a une ligne de titre : alias, point de statut, phase et latence. Le panneau actif porte la barre d'accent, les autres sont atténués. Un panneau en échec a un point plein et sa phase en couleur d'erreur, dans son titre comme dans l'onglet ; le point creux est réservé à un panneau sans shell (sorti, fermé). Le zoom affiche le panneau actif seul ; changer de panneau, diviser ou fermer y met fin, et diviser un panneau zoomé dézoome puis divise. Une division impossible est refusée sur une ligne de la barre de statut, pendant 4 s, sans couvrir aucun panneau (« This tab already has 8 panes · Close a pane first. », « Each pane needs at least 20 columns × 5 rows · Enlarge the window, or close a pane. »). Depuis un onglet divisé, `?` ouvre l'aide sur la page « Panes ».

### TUI : hôtes créés dans Portico

« Add host… » (`a` ou `n`), « Edit host… » (`e`, hôte Portico), « Duplicate as Portico host… » (`D`) et « Delete host » (`Del`) depuis la liste. Ces hôtes vivent dans le fragment `hosts.conf` de Portico ; Portico n'écrit jamais `~/.ssh/config`. Titres : « Add host », « Edit <alias> », « Duplicate <alias> » (sans points de suspension). Champs : alias, `HostName`, `User`, `Port`, clé (choisie parmi les clés locales découvertes, ou « none · agent and default keys »), `ProxyJump` (un hôte existant, ou « none »), mot de passe facultatif juste après (masqué, mémorisé dans le trousseau ou le vault), tags, groupe, favori, notes. À l'édition, le formulaire dit « A password is remembered » (`Keep`, `Change` ou `Forget`) ou « No password remembered » (seul « Set », champ vide). Sous un mot de passe saisi ou gardé, une note dit où il part : « Sent automatically to the server that answers at this address, after its host key is verified; then tied to that key. » ; si `HostName`, `User` ou `Port` changent, elle devient « Changing the server or user forgets the remembered password. Choose Change to set one for the new destination. » Sans vault (ou vault verrouillé), enregistrer un mot de passe ouvre « Create the vault » (ou « Unlock the vault ») puis enregistre de nouveau ; `Esc` (« not now ») revient au formulaire, le mot de passe toujours saisi et le refus sous le champ. Chaque valeur est vérifiée par le moteur : un refus s'affiche sous le champ concerné (choisi par son code d'erreur), avec son hint (sauf pour le port, dont le message suffit). Un hôte Portico issu d'un import l'indique (« Portico · from Termix » dans les détails et le titre) et son formulaire précise : « Editing it keeps later imports from updating it. » Un hôte de `~/.ssh/config` s'ouvre en lecture seule : « Defined in ~/.ssh/config, which Portico never edits. Duplicate it as a Portico host to change it. » ; un hôte de l'ancien fichier d'import : « Imported by an earlier version of Portico (imports/termius.conf), which it never edits. Duplicate it as a Portico host to change it. » (`D` duplicate, `t` tags and notes).

| Action | Touches |
|---|---|
| Champ suivant / précédent | `Tab` / `Shift-Tab`, `↓` / `↑` |
| Choisir (clé, `ProxyJump`, favori, mot de passe) | `←` / `→`, `Space` ; une lettre saute au choix suivant qui commence par elle |
| Nouvelle ligne dans les notes | `Alt-↵` |
| Enregistrer | `↵` (ou `Ctrl-S`) |
| Vider le champ | `Ctrl-U` |
| Annuler (avec des changements : `y` les abandonne, `n` ou `Esc` revient au formulaire, les autres touches sont ignorées) | `Esc`, `Ctrl-C` |
| Supprimer (« Delete <alias> (user@host:port) and its tags and notes. Its remembered password is forgotten unless another host uses the same account. Open sessions stay connected. This cannot be undone. » ; confirmation armée : touches ignorées 400 ms, puis seul `y` supprime ; `n` ou `Esc` annule, les autres touches sont ignorées). Sur un hôte non Portico, un toast « <alias> is read-only » (casse de l'alias conservée) | `Del` depuis la liste |

Après un enregistrement, la liste se recharge (`HostsChanged`) et sélectionne l'hôte. Les hôtes Portico portent `◆` dans la liste, les hôtes importés `◇` ; les détails indiquent l'origine. Tant que `~/.ssh/config` n'inclut pas `hosts.conf`, un bandeau sous les détails propose la ligne `Include` (« Optional: for ssh, scp and git to see these hosts too, add this line at the top of ~/.ssh/config: » ; `y` la copie, `z` masque le bandeau jusqu'au prochain lancement).

### TUI : tags, groupe et notes d'un hôte en lecture seule

« Edit <alias> » (`e` puis `t` sur un hôte de `~/.ssh/config` ou importé). Tags, groupe et notes vivent dans `hosts.toml` ; Portico n'écrit jamais `~/.ssh/config`. Les tags se tapent librement, séparés par des espaces ou des virgules, `#` facultatif, et s'affichent en chips ; ils sont validés dès la saisie avec les règles du moteur : le tag refusé est marqué en couleur d'erreur et la raison s'affiche sous le champ, avec son hint.

| Action | Touches |
|---|---|
| Champ suivant / précédent | `Tab` / `Shift-Tab` (`↓` / `↑` hors notes) |
| Nouvelle ligne dans les notes | `↵` |
| Enregistrer | `Ctrl-S` (ou `↵` hors notes) |
| Vider le champ | `Ctrl-U` |
| Annuler | `Esc`, `Ctrl-C` |

Les notes s'affichent dans le panneau de détails de l'hôte.

### TUI : journaux de session (panneau debug)

« Session logs » : `l` depuis la liste, `Ctrl-Space l` partout (depuis un onglet terminal, sa session est présélectionnée). À gauche, les sessions récentes (vivantes, puis les 20 dernières terminées), issue colorée, sondes de copy-id marquées « probe » ; sous 100 colonnes, cette colonne devient un sélecteur d'une ligne (« ◂ lab · 22:23 (1/3) ▸ »). À droite, deux onglets : **Logs** (enregistrements : niveau, cible, message ; suivi en direct ; la vue commence toujours sur un début d'enregistrement) et **Effective config** (« Config » si la largeur manque ; config effective de l'alias, comme `ssh -G` : route « You → bastion → hôte », chaque option avec son origine `fichier:ligne`, « default » ou « at connect », avertissements). Couleurs : WARN et ERROR en couleur de statut, INFO secondaire, DEBUG / TRACE atténués.

| Action | Touches |
|---|---|
| Session précédente / suivante | `↑` / `↓`, `k` / `j`, `[` / `]`, `←` / `→` |
| Onglet Logs / Effective config | `Tab` |
| Filtrer les enregistrements (texte) | `/`, saisie, `↵` ; `Esc` vide le filtre |
| Niveau minimal (debug → info → warn → error) | `v` |
| Copier (OSC 52, texte `to_line` ; l'onglet Effective config ne copie jamais les options sensibles) | `y` |
| Révéler / masquer les options sensibles (`ProxyCommand`, `LocalCommand`, `RemoteCommand`, `KnownHostsCommand` : « ••• hidden · s reveals · never copied », révélées sur cet écran seulement, jamais copiées) | `s` (onglet Effective config) |
| Défiler / plus ancien / plus récent (`G` reprend le suivi) | `PgUp` / `PgDn`, `g` / `G` |
| Rafraîchir | `Ctrl-R` |
| Aide (par-dessus le panneau) | `?` |
| Fermer | `Esc`, `q`, `Ctrl-C` |

Le panneau de détails d'un hôte masque aussi ces options (« ••• hidden · l reveals in logs »). Quand une connexion échoue, ses détails proposent `↵ retry` et `l logs`.

### TUI : explorateur de fichiers

Deux panneaux, local à gauche et distant à droite, sur la session de l'onglet actif (ou de l'hôte sélectionné). Le panneau actif a la bordure d'accent. La file des transferts s'affiche en bas tant qu'elle n'est pas vide.

| Action | Touches |
|---|---|
| « Switch side (local/remote) » : changer de côté | `Tab` / `Shift-Tab` |
| Se déplacer | `j` / `k`, `↓` / `↑`, `PgUp` / `PgDn`, `g` / `G` |
| Ouvrir le dossier / remonter | `↵`, `→`, `l` / `Backspace`, `←`, `h` |
| Copier la sélection vers l'autre panneau (download ou upload) | `c` ou `F5` |
| Nouveau dossier | `n` ou `F7` |
| Renommer | `r` ou `F2` |
| Supprimer (confirmation `y` / `n`) | `d`, `F8` ou `Suppr` |
| Annuler le dernier transfert en cours | `x` |
| Afficher / masquer les fichiers cachés | `.` |
| Rafraîchir les deux panneaux | `Ctrl-R` |
| Destination existante | `k` garder les deux (`nom (1).ext`) · `o` écraser · `s` ignorer · `Esc` annuler |
| Session distante pas prête / terminée | le panneau distant affiche la phase, ou l'erreur et son hint ; `↵` reconnecte puis rouvre SFTP |
| Aide | `?` |
| Revenir (terminal ou liste des hôtes) | `Esc`, `q`, `Ctrl-C` |

### TUI : tunnels

Panneau superposé : tunnels actifs de toutes les sessions, puis ceux déclarés dans `ssh_config` (`LocalForward`, `RemoteForward`, `DynamicForward`) pour l'hôte courant, proposés mais jamais démarrés d'office.

| Action | Touches |
|---|---|
| Ajouter (`-L 8080:db:5432`, `-R 9000:localhost:3000`, `-D 1080` pour un proxy SOCKS5) | `a` (ou `n`), saisie, `↵` |
| Démarrer un tunnel proposé | `↵` |
| Arrêter le tunnel sélectionné | `x`, `d` ou `Suppr` |
| Confirmer une écoute exposée (adresse autre que loopback) | `y` (toute autre touche annule) |
| Fermer | `Esc`, `q`, `Ctrl-C` |

### TUI : clés

Panneau en deux sections : clés locales (`~/.ssh`), puis identités de l'agent ; statut du vault en pied (seulement si le keyring système n'est pas disponible). Seules les touches utilisables sur la sélection sont proposées.

| Action | Touches |
|---|---|
| Se déplacer / changer de section | `j` / `k`, `↓` / `↑`, `g` / `G` · `Tab` |
| Nouvelle clé (formulaire) | `n` |
| Ajouter à l'agent (durée : « Until removed » / 1 h / 8 h, passphrase si besoin) | `a`, `←` `→`, `↵` |
| Retirer de l'agent (confirmation) | `x` (ou `Suppr`), puis `y` |
| Copier la clé sur un hôte (sélecteur flou, puis Connecting · Installing · Verifying ; sans clé privée, `.pub` seul, l'étape Verifying est sautée et le résultat le dit) | `c`, saisie, `↵` |
| Réessayer une copie en échec / quitter la vue (la copie continue) | `↵` ou `r` / `Esc` |
| Copier la clé publique (OSC 52, et la ligne s'affiche) | `y` (`Esc` masque la ligne) |
| Vault : déverrouiller ou verrouiller | `v` |
| Vault : créer, ou changer la passphrase | `V` |
| Rafraîchir | `Ctrl-R` |
| Fermer | `Esc`, `q`, `Ctrl-C` |

Quand la liste dépasse la hauteur, elle défile avec la sélection (l'en-tête de section reste visible sur sa première ligne) et « ↑ n more / ↓ n more » compte les clés masquées. Pendant une question en ligne (durée, retrait), la ligne sélectionnée garde son fond. Une notice qui ne tient pas sous la liste s'affiche en toast sous le panneau.

Formulaire de génération : `Tab` / `Shift-Tab` (ou `↓` / `↑`) entre les champs, `←` `→` ou `Espace` sur le type, `Ctrl-U` vide le champ, `↵` génère (les erreurs s'affichent sous le champ), `Esc` annule. La passphrase fait au moins 8 caractères, ou est vide (compteur « n / 8 min »). Si le moteur refuse la génération alors qu'une passphrase avait été envoyée, les champs sont effacés : `↵` reste bloqué tant qu'elle n'est pas ressaisie, ou que `Ctrl-N` ne confirme « sans passphrase ».

### TUI : snippets

Liste des snippets qui s'appliquent à l'hôte de l'onglet actif (portée affichée : `global`, `host web`, `#tag`). La frappe filtre la liste, d'où les commandes en `Ctrl`. Sans onglet terminal ouvert sur l'hôte, l'exécution est désactivée (le panneau l'explique) ; la gestion reste possible.

| Action | Touches |
|---|---|
| Filtrer / vider le filtre | saisie / `Ctrl-U` (ou `Esc` si le filtre n'est pas vide) |
| Se déplacer | `↓` / `↑`, `PgUp` / `PgDn` |
| Exécuter (« run » : tapé puis Entrée) ou coller (« paste » : tapé seulement) dans l'onglet ; formulaire si `{{placeholders}}` | `↵` |
| Nouveau / éditer | `Ctrl-N` / `Ctrl-E` |
| Supprimer (confirmation) | `Ctrl-D` (ou `Suppr`), puis `y` |
| Recharger | `Ctrl-R` |
| Fermer | `Esc`, `Ctrl-C` |

Sans terminal ouvert, le panneau l'indique (« No open terminal on <hôte> ») avec l'étape suivante : `Esc`, puis `↵` pour se connecter.

Formulaire des placeholders : un champ par `{{nom}}`, `Tab` entre les champs, aperçu de ce qui sera tapé, `↵` exécute ou colle (erreurs en ligne), `Esc` revient. Éditeur : `Tab` entre les champs, `↵` dans la commande insère une ligne, `←` `→` change la portée, `Espace` bascule « Run » (« yes — Enter is sent » / « no — typed only »), `Ctrl-S` (ou `↵` hors commande) enregistre, `Esc` annule. Un snippet multi-ligne « paste only » refusé (le programme distant n'accepte pas le collage groupé) demande confirmation : seul `r` l'exécute quand même (touches ignorées pendant 400 ms, `↵` ne fait rien), `Esc` annule.

### TUI : enregistrements et lecture

« Recordings » : `v` depuis la liste, `Ctrl-Space v` partout. Les fichiers asciicast v2 du dossier `recordings/`, du plus récent au plus ancien : alias, début en heure locale sur 24 h (`22 Oct 13:34`, l'année seulement si ce n'est pas l'année en cours ; suffixe « UTC » si le fuseau local n'a pas pu être lu au démarrage), durée (`m:ss` ou `h:mm:ss`), taille ; `●` en couleur d'erreur pour un fichier en cours d'écriture, `▲` en couleur d'avertissement pour un fichier invalide (non lisible, mais supprimable : « Not a valid asciicast v2 recording: it cannot be played, it can be deleted. » s'affiche quand il est sélectionné). Le nom du fichier sélectionné s'affiche sous la liste ; les touches sont dans la barre de statut.

| Action | Touches |
|---|---|
| Se déplacer | `j` / `k`, `↓` / `↑`, `PgUp` / `PgDn`, `g` / `G` |
| Lire l'enregistrement | `↵` |
| Supprimer (« The file is removed from the recordings folder. This cannot be undone. » ; confirmation armée : touches ignorées 400 ms, puis seul `y` supprime, `n` ou `Esc` garde le fichier, les autres touches sont ignorées). Un enregistrement en cours d'écriture est refusé d'emblée (« This recording is still being written » · « Stop the recording first ») et `d` n'est pas proposé sur sa ligne | `d` ou `Suppr`, puis `y` |
| Rafraîchir | `Ctrl-R` |
| Fermer | `Esc`, `q`, `Ctrl-C` |
| Lecture : pause / reprise ; une fois terminée, rejouer depuis le début | `Espace` |
| Lecture : vitesse (×0,5 à ×8) | `+` / `-` |
| Lecture : sauter le silence en cours | `→` |
| Lecture : revenir de 5 s | `←` |
| Lecture : reprendre au début | `Home` |
| Lecture : revenir à la liste | `q`, `Esc` (`Ctrl-C` ferme tout) |

Le lecteur occupe le corps de l'écran : une ligne de titre (titre de l'en-tête, toujours suivi du nom du fichier, car un fichier tiers peut porter n'importe quel titre ; puis l'état et la vitesse), la grille enregistrée dans un émulateur dédié, puis la progression « temps / total » (jauge en accent) et la taille, ou ce qui ne tient pas (« 2 rows hidden »). Sur un écran de 24 lignes, la ligne de titre disparaît : nom du fichier, état et vitesse passent sur la ligne de progression, et la grille gagne une ligne. `←` et `Home` rouvrent le fichier et rejouent instantanément jusqu'au point visé (l'émulateur repart de zéro). Les silences de plus de 2 s sont raccourcis. Une erreur de lecture s'affiche à la place de la jauge (message · conseil). Le statut de la session (`● alias · latence`) et `● REC` restent visibles dans la barre de statut. Aucune touche ni aucun collage ne part vers un terminal pendant la lecture.
<!-- END GENERATED: tui -->

### Panneaux de la TUI, en pratique

Un onglet terminal se partage en panneaux, comme dans tmux, mais chaque panneau est un canal PTY de Portico :
`Ctrl-Space %` (ou `|`, « Split right ») ouvre un panneau à droite, `Ctrl-Space "` (ou `-`, « Split down ») un
panneau dessous, tous deux sur la session du panneau actif, donc sans nouvelle authentification.
`Ctrl-Space S` (« Split with host… ») ouvre la liste des hôtes pour diviser vers un autre hôte : on tape son nom,
le panneau de détails annonce `↵ split with <hôte>`, `↵` choisit, `Esc` revient au panneau ; une session vivante
vers cet hôte est réutilisée, sinon Portico s'y connecte. Le focus se déplace avec `Ctrl-Space` puis les flèches
(`o`, « Next pane », pour le panneau suivant), la séparation avec `Ctrl-Space` puis `Shift` + flèche
(« Move divider »), `z` zoome le panneau actif (« Zoom pane ») et `x` le ferme (« Close pane »).

Comme le `repeat-time` de tmux, un déplacement ouvre le **mode `PANE`** : pendant 600 ms après `Ctrl-Space` + flèche
(ou `Shift` + flèche), les flèches et `Shift` + flèches agissent sans repasser par le préfixe, et chaque nouveau
déplacement relance le délai ; la barre de statut affiche `PANE`. Toute autre touche quitte le mode et part au
panneau actif comme d'habitude : aucune frappe n'est avalée. Seules les flèches se répètent (pas les touches
réassignées).

Les lettres `h` et `l` du préfixe restant « Retour aux hôtes » et « Logs », les déplacements à la vim
s'obtiennent en les réassignant dans `settings.toml` (par exemple `"prefix.home" = "0"`, `"prefix.debug" = "g"`,
puis `"prefix.pane_left" = "h"`, `"prefix.pane_right" = "l"`, `"prefix.pane_down" = "j"`,
`"prefix.pane_up" = "k"`). Seul le panneau actif reçoit le clavier ; fermer le dernier panneau d'une session la
déconnecte, comme fermer le dernier onglet. Un panneau dont la connexion échoue affiche l'erreur, son hint et
`↵ retry` quand il a le focus ; les autres indiquent « focus to retry (Ctrl-Space ←↓↑→) », puisque `↵` va au
panneau actif. Une division refusée s'affiche sur une ligne de la barre de statut (4 s), jamais par-dessus un
panneau ; diviser un panneau zoomé dézoome puis divise. Depuis un onglet divisé, `Ctrl-Space ?` ouvre l'aide sur
la page « Panes ».

### Ajouter un hôte dans la TUI

Depuis la liste, `a` (ou `n`, « Add host… ») ouvre le formulaire : alias, `HostName`, `User` et `Port` se
tapent, la clé se choisit parmi les clés privées découvertes dans `~/.ssh` (ou « none ») et le `ProxyJump` parmi
les hôtes existants (ou « none ») avec `←` / `→` ; une lettre saute au choix suivant qui commence par elle.
Viennent ensuite tags, groupe, favori, notes (`Alt-↵` pour une nouvelle ligne) et un mot de passe facultatif,
jamais affiché (masque fixe) et mémorisé dans le trousseau du système, ou dans le vault de Portico ; Portico le
propose à la première connexion seulement après avoir vérifié la clé du serveur. `↵` enregistre ; un refus du
moteur s'affiche sous le champ qu'il concerne, avec quoi faire. `Esc` annule, et demande confirmation (`y`) si
quelque chose a été saisi ; le mot de passe tapé est alors effacé. L'hôte rejoint la liste aussitôt, sélectionné,
marqué `◆` : il vit dans le fichier `hosts.conf` de Portico, jamais dans `~/.ssh/config`. Pour que `ssh`, `scp`
et `git` le voient aussi, un bandeau sous les détails donne la ligne `Include` à ajouter soi-même en tête de
`~/.ssh/config` (`y` la copie, `z` masque le bandeau jusqu'au prochain lancement). `e` modifie un hôte Portico
(renommer compris ; le mot de passe : `Keep`, `Change` ou `Forget`), `D` duplique n'importe quel hôte en hôte
Portico sous un nouvel alias, `Del` en supprime un après une confirmation armée (seul `y` supprime). Sur un hôte
de `~/.ssh/config`, `e` explique qu'il est en lecture seule et propose `D` pour le dupliquer, ou `t` pour éditer
ses tags, son groupe et ses notes.

### Enregistrer une session dans la TUI

`Ctrl-Space R` (« Record session ») enregistre le panneau actif au format asciicast v2. Une confirmation
« Record this terminal? » (« Record this pane? » dans un onglet partagé, avec l'alias et « pane n/N ») dit
exactement : « Everything the terminal shows is recorded, including the commands you type (the shell echoes them)
and any secret a command prints. Keystrokes themselves are not stored, so a password typed at a prompt that does
not echo stays out. Saved to <dossier>, readable only by you. » Le dossier est `recordings/` des données de
Portico, `~` pour le dossier personnel, le milieu coupé s'il est long. Les touches tapées dans les 400 ms qui
suivent son ouverture sont ignorées (`y record` reste atténué, puis passe en accent quand il devient actif) ;
ensuite seul `y` démarre, `n` ou `Esc` annule, et les autres touches sont ignorées. Le badge `REC` (couleur
d'erreur) s'allume sur le titre du panneau, sur l'onglet dès qu'un de ses panneaux est enregistré
(`1 alpha ● · REC`, séparé du point de connexion) et `● REC` dans la barre de statut, dès que le moteur annonce le
début de l'enregistrement, et s'éteint quand il en annonce la fin (jamais sur la seule réponse de la commande). La
même touche l'arrête, sans confirmation (« stop recording » dans la barre de statut) ; la barre le confirme
pendant 4 s, en texte neutre : « Recording saved · <fichier> » · « Ctrl-Space v to open Recordings ». Si
l'enregistrement s'arrête de lui-même, une ligne de la barre de statut le dit, avec quoi faire (« Recording
stopped: the disk could not keep up », « … the file could not be written », « … size limit reached (512 MiB) »).
Le statut de la session (`● alias · latence`) et `● REC` gardent toujours leur place à droite de la barre : sur
une barre courte, l'alias est raccourci (12 caractères), puis les raccourcis les moins utiles disparaissent.
`v` (liste des hôtes) ou `Ctrl-Space v` ouvre « Recordings » pour relire ou supprimer ; un enregistrement en
cours d'écriture ne peut pas être supprimé (refus immédiat, sans question). Dans le lecteur : `Espace` pause,
reprend, ou rejoue depuis le début une fois terminé ; `+` / `-` la vitesse ; `→` saute le silence en cours ; `←`
revient de 5 s ; `Home` reprend au début ; `q` ou `Esc` revient à la liste. Un fichier relu est traité comme non fiable : il est rejoué dans un émulateur
dédié dont les réponses aux requêtes, le titre et le presse-papiers ne vont nulle part, ses modes (souris,
collage, écran alternatif) ne touchent pas le vrai terminal, et aucune touche ni aucun collage ne part vers une
session pendant la lecture.

## Dialogues

| Dialogue | Touches |
|---|---|
| Nouvelle clé d'hôte | `y` faire confiance et enregistrer · `n` / `Esc` refuser |
| Clé d'hôte inhabituelle (autre type déjà connu) | taper `yes` puis `↵` · `Esc` refuser |
| Mot de passe / passphrase | saisie masquée · `Tab` champ suivant / case « mémoriser » · `Espace` cocher · `↵` valider · `Esc` / `Ctrl-C` annuler |
| Keyboard-interactive | `Tab` entre les champs · `↵` valider · `Esc` annuler |
| Clé d'hôte modifiée (alerte) | `Esc` / `↵` fermer (aucune acceptation possible) |
| Vault verrouillé au démarrage (TUI) | saisie masquée · `↵` déverrouiller · `Esc` continuer sans (secrets non mémorisés ; la barre de statut affiche « Esc continue without ») |
| Créer le vault / changer sa passphrase (TUI) | `Tab` champ suivant · `↵` valider · `Esc` annuler (« not now » quand il est proposé après « mémoriser ») ; au moins 8 caractères, compteur « n / 8 min » dès l'ouverture |
| Passphrase d'une clé pour l'agent (TUI) | saisie masquée · `↵` ajouter · `Esc` annuler |

## Raccourcis configurables (GUI)

Les tableaux GUI ci-dessous sont **générés** depuis la table de commandes de la GUI
(`apps/portico-gui/src/lib/shortcuts.ts`, touches fixes dans `keybindings-doc.ts`) par
`pnpm gen:keybindings` ; `pnpm check` échoue si ce fichier est périmé. Chaque action a un identifiant
stable, utilisable dans `settings.toml` (dossier de configuration de Portico : `~/.config/portico/` sous
Linux, `~/Library/Application Support/dev.Portico.portico/` sous macOS) :

```toml
theme = "basalt"                 # obsidian, basalt, travertine ; mémorisé quand on change de thème

[keymap.gui]
"app.files" = "Mod+Shift+O"      # remplace ⌘⇧F / Ctrl-Shift-F
"app.logs" = "F12"
"hosts.clear-recent" = "Mod+Shift+Backspace"   # action de palette sans touche par défaut
"app.reload" = "none"            # retire le raccourci
```

- Syntaxe : parties séparées par `+`, sans casse. Modificateurs `Mod` (⌘ sur macOS, `Ctrl` ailleurs, et
  `Ctrl-Shift` dès qu'un terminal a le focus), `Ctrl`, `Alt` (`Option`), `Shift`, `Cmd` (`Meta`, `Super`).
  Touches : `a`–`z`, `0`–`9`, `F1`–`F12`, `Enter`, `Esc`, `Tab`, `Space`, `Backspace`, `Delete`, `Insert`,
  `Up` `Down` `Left` `Right`, `Home`, `End`, `PageUp`, `PageDown`, et les signes `/` `[` `]` `.` `,` `;` `'` `-` `=` `?` `*` `\` et `` ` ``. `none` retire.
- Une entrée remplace toutes les combinaisons par défaut de l'action. Les actions globales et de panneau exigent `Mod`,
  `Ctrl`, `Alt` ou une touche `F` ; les actions de la liste des hôtes acceptent une touche seule.
- Entrées ignorées (le défaut reste) et listées dans une notification au démarrage : action inconnue,
  combinaison illisible, conflit avec une autre action, combinaison réservée (copier/coller du terminal
  `Ctrl-Shift-C` / `Ctrl-Shift-V` / `Shift-Insert`, menu Édition et `⌘Q` / `⌘H` / `⌘M` sur macOS,
  `Ctrl-<touche>` seul dans un terminal, `Esc` et `Tab`), action fixe (`tab.copy`, `tab.paste`).
- Un `settings.toml` invalide (au sens du moteur) est ignoré entièrement et signalé ; Portico ne le réécrit
  pas. La palette, les infobulles et la barre de statut affichent les combinaisons effectives.
- Le thème est appliqué avant le premier rendu (dernier thème mémorisé localement, confirmé par
  `settings.toml`) ; un nom inconnu donne le thème par défaut et une notification.
- Les actions de panneau (`pane.*`) n'écoutent que dans un onglet terminal : elles peuvent reprendre une
  combinaison de la liste des hôtes (`Mod+D` divise dans un terminal et bascule le favori dans la liste).

## Panneaux (GUI) : principe

Un onglet se divise en panneaux (8 au plus), chacun un terminal indépendant : « Split pane right » / « Split pane
down » ouvrent un canal PTY de plus sur la **même session** (aucune nouvelle connexion), « Split pane with host… »
ouvre le sélecteur d'hôtes (récents, puis favoris, puis les autres, chacun avec son point de statut et
`user@host` ; « reuses connection » signale un hôte dont la session vivante sera réutilisée) et place un terminal
d'un **autre hôte** à côté. Diviser un onglet zoomé annule d'abord le zoom. Un seul panneau a le focus (liseré
d'accent, barre d'accent en tête) et lui seul reçoit les frappes et affiche un curseur ; les autres sont atténués.
Chaque panneau affiche son statut (point, alias, phase, latence) dans son en-tête ; un panneau en échec a un point
plein et une phase en couleur d'erreur, et son encart propose `Retry ↵` et `Close pane`. La barre d'état parle du
panneau actif (« pane 2/4 ») et ses indications deviennent « panes », « close pane », « split ». Une division
impossible est refusée par une notification en haut à droite de la zone des terminaux : « This tab already has
8 panes » (« Close a pane first. ») ou « Each pane needs at least 20 columns × 5 rows » (« Enlarge the window,
or close a pane. »). `pane.divider` (« Move pane divider… ») donne le focus à la séparation la plus proche du
panneau actif : flèches pour la déplacer, `Home` / `End` jusqu'aux limites, `↵` / `Esc` pour rendre le clavier ;
la souris la fait glisser. Chaque terminal suit sa taille (resize PTY après un court délai). Le zoom donne tout
l'onglet au panneau actif sans fermer les autres. `tab.close` (`⌘W` / `Ctrl-Shift-W`) ferme le panneau actif d'un
onglet divisé, `tab.close-all` (`⌘⇧W` / `Ctrl-Alt-Shift-W` : sur Linux, `Ctrl-Shift-W` est déjà `tab.close`
dans un terminal) ferme l'onglet et tous ses panneaux ; `pane.close` reste dans la palette, sans touche. Fermer un panneau ferme son canal PTY ; le dernier panneau d'un onglet ferme
l'onglet, et le dernier panneau d'une session la déconnecte, comme le dernier onglet.

## Enregistrement de sessions (GUI) : principe

Enregistrer est un choix explicite, **par panneau** : « Record session » (`session.record`, `⌘⇧R` / `Ctrl-Shift-R`
dans un terminal, ou la palette) ouvre la confirmation « Record this terminal? » (« Record this pane? » dans un
onglet divisé, avec l'alias et « pane n/N »). Son texte, identique dans la TUI : « Everything the terminal shows is
recorded, including the commands you type (the shell echoes them) and any secret a command prints. Keystrokes
themselves are not stored, so a password typed at a prompt that does not echo stays out. Saved to <dossier>, readable
only by you. » Seul `y` démarre, et seulement après 400 ms sans frappe (bouton « Record y ») ; `Esc` et `n`
annulent, `↵` ne fait rien. Un badge `REC` (couleur d'erreur) apparaît sur l'en-tête du panneau, sur l'onglet et
dans la barre d'état **à l'événement `started` du moteur** et disparaît à `stopped`, jamais sur la seule réponse de
la commande ; après une perte d'événements, l'état est relu auprès du moteur (et un arrêt que le moteur dit déjà
fait éteint `REC`). Tant que le panneau actif est enregistré, la barre d'état rappelle « `⌘⇧R` / `Ctrl-Shift-R`
stop ». Arrêter (`session.record` à nouveau, « Stop recording ») se fait sans confirmation et affiche 4 s une notice
neutre « Recording saved · <fichier> » (« v from the host list opens Recordings »). Un arrêt involontaire est
signalé : « Recording stopped: the disk could not keep up », « … the file could not be written », « … size limit
reached (512 MiB) ». « Recordings… » (`v` depuis la liste des hôtes, ou la palette) liste les fichiers (titre de
l'en-tête **et** nom de fichier, toujours affiché car un fichier tiers peut porter n'importe quel titre ; date
locale sur 24 h, `22 Oct 13:34`, l'année seulement si elle diffère ; durée en horloge `m:ss` / `h:mm:ss` ; taille).
Un fichier invalide porte « invalid » en couleur d'avertissement et, sélectionné : « Not a valid asciicast v2
recording: it cannot be played, it can be deleted. » La suppression est confirmée (« The file is removed from the
recordings folder. This cannot be undone. ») et refusée d'emblée tant que le fichier s'écrit (« This recording is
still being written » · « Stop the recording first » ; le bouton Delete est désactivé). La lecture se fait dans une
instance xterm.js dédiée, en lecture seule : la sortie est assainie côté Rust (OSC hors couleurs, DCS/APC, C1
retirés), le terminal n'a ni entrée, ni liens, ni presse-papiers, ni réponse aux requêtes (`ESC[6n`, `ESC[c`), et
les modes que joue l'enregistrement (souris, bracketed paste, focus) restent dans le lecteur. Touches du lecteur,
identiques dans la TUI : `Espace` lecture / pause (à la fin : rejouer), `+` / `-` vitesse, `→` sauter le silence,
`←` reculer de 5 s, `Home` revenir au début, `q` et `Esc` fermer ; à la fin : « Space replay · q back to list ».
« Show in folder » n'existe pas : il faudrait un plugin d'ouverture et une capability de plus.

## Hôtes créés dans Portico (GUI) : principe

Les hôtes créés depuis la GUI vivent dans le fichier de Portico, `hosts.conf` (ADR « Hôtes créés dans Portico ») ;
`~/.ssh/config` n'est jamais modifié. « Add host… » (`host.add`, `⌘⇧N` / `Ctrl-Shift-N`, partout, ou le bouton
« + » à côté de la recherche) ouvre le formulaire ; « Edit host… » (`host.edit`, `⇧E`) le rouvre pour un hôte
Portico, et `e` (`hosts.edit`) aussi : sur un hôte Portico, il édite tout l'hôte, sur les autres seulement tags,
groupe et notes. Sur un hôte de `~/.ssh/config` (ou importé de Termius), « Edit host… » affiche « Defined in
~/.ssh/config, which Portico never edits. Duplicate it as a Portico host to change it. » et propose « Duplicate as
Portico host… » (`host.duplicate`, `⇧D`), qui demande un nouvel alias puis ouvre le formulaire de la copie.
« Delete host » (`host.delete`, `⌘⌫` / `Suppr`, jamais pendant la saisie dans un champ) demande `y` / `n` ; un hôte
qui sert de rebond est refusé avec la raison. La clé se choisit parmi celles que le moteur a trouvées dans `~/.ssh`
(la GUI n'envoie qu'un identifiant, jamais un chemin) ; un mot de passe facultatif est confié au trousseau (ou au
vault) et le champ est vidé après l'envoi. Origines dans la liste et le détail : badge « Portico », « Imported »,
et « ~/.ssh/config » (cadenas) dans le détail. Tant qu'un hôte Portico existe sans que `~/.ssh/config` l'inclue, le
détail affiche « Optional: for ssh, scp and git to see these hosts too, add this line at the top of ~/.ssh/config: »,
la ligne `Include …` et « Copy » (aussi dans la palette) ; le bandeau se ferme et ne revient pas pour la même ligne.
Sur un hôte de `~/.ssh/config` ou importé, la vue en lecture seule (aussi ouverte par « Delete host », titrée
« <alias> can't be deleted here ») propose `D` (dupliquer) et `t` (tags et notes), comme la TUI.

<!-- BEGIN GENERATED: gui -->
<!-- Généré par `pnpm gen:keybindings` (apps/portico-gui/src/lib/shortcuts.ts et keybindings-doc.ts) : ne pas modifier à la main. -->

## Application et onglets (GUI)

Actions globales, actives partout (terminal compris). Sur Linux, `Ctrl-Shift-…` est affiché parce qu'il marche partout ;
dans la liste des hôtes, `Ctrl-…` seul suffit (`Ctrl-K`, `Ctrl-D`). Un « — » : pas de touche par défaut, l'action reste
dans la palette et peut recevoir une combinaison dans `settings.toml`.

| Action | Identifiant | macOS | Linux |
|---|---|---|---|
| Palette de commandes | `app.palette` | `⌘K` | `Ctrl-Shift-K` |
| Fichiers (SFTP) de l'hôte, à côté du terminal (ouvre ou ferme le panneau) | `app.files` | `⌘⇧F` | `Ctrl-Shift-F` |
| Tunnels | `app.tunnels` | `⌘⇧P` | `Ctrl-Shift-P` |
| Clés (locales, agent, vault) | `app.keys` | `⌘⇧I` | `Ctrl-Shift-I` |
| Snippets de l'hôte (la palette liste aussi « Run: <nom> ») | `app.snippets` | `⌘⇧S` | `Ctrl-Shift-S` |
| Journaux de session et config effective (panneau debug) | `app.logs` | `⌘⇧L` | `Ctrl-Shift-L` |
| Nouvel onglet sur le même hôte (depuis la liste : se connecter) | `tab.new` | `⌘T` | `Ctrl-Shift-T` |
| Fermer l'onglet ; dans un onglet divisé, le panneau actif (le dernier panneau ferme l'onglet ; le dernier onglet d'un hôte le déconnecte ; pendant la connexion : annuler) | `tab.close` | `⌘W` | `Ctrl-Shift-W` |
| Fermer l'onglet et tous ses panneaux | `tab.close-all` | `⌘⇧W` | `Ctrl-Alt-Shift-W` |
| Onglet suivant | `tab.next` | `Ctrl-Tab`, `⌘]` | `Ctrl-Tab` |
| Onglet précédent | `tab.prev` | `Ctrl-Shift-Tab`, `⌘[` | `Ctrl-Shift-Tab` |
| Liste des hôtes | `tab.hosts` | `⌘0` | `Alt-0` |
| Aller à l'onglet n (`tab.goto-1` … `tab.goto-9`) | `tab.goto-1` | `⌘1` | `Alt-1` |
| Copier la sélection du terminal (macOS : menu Édition, `⌘C`) | `tab.copy` (fixe) | — | `Ctrl-Shift-C` |
| Coller (macOS : `⌘V` ; Linux aussi `Shift-Insert`, clic milieu) | `tab.paste` (fixe) | — | `Ctrl-Shift-V` |
| Reconnecter l'hôte de l'onglet | `tab.reconnect` | — | — |
| Déconnecter l'hôte de l'onglet | `tab.disconnect` | — | — |
| Ajouter un hôte (dans `hosts.conf` de Portico, jamais `~/.ssh/config`) ; aussi le bouton « + » de la liste | `host.add` | `⌘⇧N` | `Ctrl-Shift-N` |
| Enregistrements : liste, lecture et suppression (touches : tableau « Enregistrements (GUI) »). Sans raccourci par défaut dans un terminal : palette | `recordings.open` | — | — |
| Nouvelle clé | `keys.new` | — | — |
| Nouveau snippet | `snippets.new` | — | — |
| Changer de dossier de téléchargement | `files.download-dir` | — | — |
| Importer des hôtes Termius ou Termix (format détecté ; aperçu, clés privées seulement si cochées, puis hôtes Portico dans `hosts.conf`, voir `docs/IMPORT.md`) | `app.import-termius` | `⌘⇧M` | `Ctrl-Shift-M` |

## Panneaux (GUI)

Actives dans un onglet terminal (division, focus, taille, zoom, fermeture ; voir « Panneaux (GUI) : principe » plus haut). Sur Linux,
`Ctrl-Shift-…` comme pour les actions globales : le focus passe par `Ctrl-Shift` + flèches (`⌘⌥` + flèches sur macOS),
pas par `Ctrl-Alt` + flèches, que GNOME, Cinnamon et Xfce gardent pour les espaces de travail.

| Action | Identifiant | macOS | Linux |
|---|---|---|---|
| Diviser à droite (même session : un canal PTY de plus) | `pane.split-right` | `⌘D` | `Ctrl-Shift-D` |
| Diviser en bas (même session) | `pane.split-down` | `⌘⇧D` | `Ctrl-Shift-O` |
| Diviser avec un autre hôte (sélecteur d'hôtes ; à droite si le panneau est plus large que haut, sinon en bas) | `pane.split-host` | — | — |
| Focus au panneau voisin à gauche | `pane.focus-left` | `⌥⌘←` | `Ctrl-Shift-←` |
| Focus au panneau voisin à droite | `pane.focus-right` | `⌥⌘→` | `Ctrl-Shift-→` |
| Focus au panneau voisin vers le haut | `pane.focus-up` | `⌥⌘↑` | `Ctrl-Shift-↑` |
| Focus au panneau voisin vers le bas | `pane.focus-down` | `⌥⌘↓` | `Ctrl-Shift-↓` |
| Prendre la séparation la plus proche du panneau actif : flèches pour la déplacer, `Home`/`End` jusqu'aux limites, `↵` ou `Esc` pour rendre le clavier au panneau | `pane.divider` | `⌥⌘R` | `Ctrl-Alt-Shift-R` |
| Panneau suivant (ordre de lecture) | `pane.next` | `⌥⌘N` | `Ctrl-Alt-N` |
| Zoom on/off : le panneau actif occupe tout l'onglet | `pane.zoom` | `⌘⇧↵` | `Ctrl-Shift-↵` |
| Fermer le panneau actif et son canal PTY (le dernier panneau d'une session la déconnecte). Sans raccourci par défaut : dans un onglet divisé, `tab.close` ferme le panneau actif | `pane.close` | — | — |
| Enregistrer le panneau actif (asciicast v2, confirmation : ce qui s'affiche est enregistré, jamais ce qui est tapé) ; arrêter s'il l'est déjà (sans confirmation). `REC` sur le panneau, l'onglet et la barre d'état | `session.record` | `⌘⇧R` | `Ctrl-Shift-R` |

## Liste des hôtes (GUI)

Actives quand la liste des hôtes est affichée et que le focus n'est pas dans un champ (la recherche garde ses touches).

| Action | Identifiant | macOS | Linux |
|---|---|---|---|
| Favori on/off de l'hôte sélectionné (depuis un onglet : palette, « Add <alias> to favorites » ; dans un terminal, `Mod+D` divise le panneau) | `app.favorite` | `⌘D` | `Ctrl-D` |
| Recharger `~/.ssh/config` et `hosts.toml` | `app.reload` | `⌘R` | `Ctrl-R` |
| Rechercher (flou, `#tag`, `*` favoris, `@groupe`) | `hosts.search` | `/` | `/` |
| Hôte suivant | `hosts.down` | `j`, `↓` | `j`, `↓` |
| Hôte précédent | `hosts.up` | `k`, `↑` | `k`, `↑` |
| Se connecter (ouvre un onglet terminal, ou y revient ; aussi double-clic) | `hosts.connect` | `↵` | `↵` |
| Éditer tags, groupe et notes de l'hôte sélectionné ; un hôte Portico ouvre le formulaire complet (depuis un onglet : palette, « Edit <alias> ») | `hosts.edit` | `e` | `e` |
| Modifier un hôte Portico ; sur un hôte de `~/.ssh/config` ou importé : lecture seule, duplication proposée | `host.edit` | `⇧E` | `Shift-E` |
| Dupliquer l'hôte sélectionné comme hôte Portico (nouvel alias), puis le modifier | `host.duplicate` | `⇧D` | `Shift-D` |
| Supprimer un hôte Portico (confirmation `y` / `n`) ; refusé pour les autres | `host.delete` | `⌘Backspace` | `Del` |
| Se déconnecter (hôte sélectionné) | `hosts.disconnect` | `d` | `d` |
| Fichiers (SFTP) de l'hôte sélectionné (se connecte au besoin) | `hosts.files` | `f` | `f` |
| Tunnels | `hosts.tunnels` | `p` | `p` |
| Clés | `hosts.keys` | `i` | `i` |
| Snippets de l'hôte sélectionné | `hosts.snippets` | `s` | `s` |
| Favori on/off de l'hôte sélectionné (second raccourci de `app.favorite`, hors champ de recherche) | `hosts.favorite` | `*` | `*` |
| Retirer de « Recent » (sur une ligne de la section Recent) | `hosts.forget-recent` | `x` | `x` |
| Replier le groupe de l'hôte sélectionné | `hosts.collapse` | `←` | `←` |
| Déplier le groupe | `hosts.expand` | `→` | `→` |
| Vider « Recent » (confirmation) | `hosts.clear-recent` | `⇧X` | `Shift-X` |
| Thème suivant (mémorisé dans `settings.toml`) | `app.theme` | `t` | `t` |
| Enregistrements (second raccourci de `recordings.open`, depuis la liste des hôtes) | `hosts.recordings` | `v` | `v` |

## Terminal (GUI) : touches fixes

Toutes les touches partent vers l'hôte distant, sauf les raccourcis de l'app (tableau « Application et onglets »).

| Action | Touches |
|---|---|
| Reconnecter un onglet terminé ou en échec | `↵` |
| Annuler une connexion en cours / revenir aux hôtes (onglet sans shell ; dans un onglet divisé, `Esc` annule seulement) | `Esc` |
| Barre d'onglets (focus clavier) : onglet voisin, premier / dernier, fermer | `←` `→`, `Home` / `End`, `Suppr` |
| Fermer un onglet (souris) | clic milieu sur l'onglet |
| Coller (Linux) | `Ctrl-Shift-V`, `Shift-Insert`, clic milieu |
| Panneaux : donner le focus à un panneau (souris) | clic dans le panneau |
| Séparation entre panneaux (souris) | glisser |
| Séparation entre panneaux (focus clavier, par `pane.divider` ou `Tab`) : déplacer, aller aux limites, rendre le clavier au panneau | `←` `→` (ou `↑` `↓`), `Home` / `End`, `↵` / `Esc` |

Tout collage est assaini (ESC et contrôles C1 retirés) avant d'atteindre le terminal.

## Liste des hôtes (GUI) : sections

Sans recherche, la liste montre « Recent » (les 5 derniers hôtes encore présents dans la config), puis un titre par groupe (ordre alphabétique), puis « Other » ; sans groupe ni récent, une liste plate. Avec une recherche, les résultats classés, à plat. Les titres de section sont des lignes : on s'y déplace comme sur un hôte.

| Action | Touches |
|---|---|
| Plier / déplier la section (sur son titre) | `↵`, clic |
| Retirer de « Recent » (souris) | `×` au survol ou sur la ligne sélectionnée |
| Naviguer pendant la recherche | `↑` `↓`, `Ctrl-N` / `Ctrl-P` |
| Vider la recherche | `Esc` |
| Favori (souris) | étoile à côté du nom, dans les détails |

## Édition d'un hôte (GUI)

« Edit <alias> » : tags, groupe et notes, enregistrés dans `hosts.toml` par le moteur (Portico n'écrit jamais `~/.ssh/config`). Les erreurs s'affichent sous le champ concerné, avec un conseil.

| Action | Touches |
|---|---|
| Ajouter un tag (`#` facultatif) | `Espace`, `,` ou `↵` dans le champ Tags |
| Retirer le dernier tag | `Backspace` dans le champ vide |
| Sélectionner un tag (anneau d'accent ; le tag refusé par le moteur est marqué dès sa création) | `←` / `→` depuis le champ vide, puis `←` `→`, `Home` / `End` |
| Retirer le tag sélectionné | `Backspace` ou `Suppr` |
| Groupe : choisir une suggestion (groupes existants) | `↓` / `↑`, puis `↵` |
| Champ suivant | `Tab` |
| Enregistrer | `Ctrl-S` (macOS : `⌘S`), `Ctrl-↵`, ou `↵` dans un champ d'une ligne |
| Annuler | `Esc` |

## Hôtes Portico (GUI) : dialogues

« Add host… » et « Edit host… » (hôte Portico) : alias, HostName, User, Port, clé (parmi les clés découvertes dans `~/.ssh`), ProxyJump (hôte existant), mot de passe, tags, groupe, favori, notes. Les erreurs du moteur s'affichent sous le champ concerné, avec un conseil ; le premier champ en erreur prend le focus.

| Action | Touches |
|---|---|
| Champ suivant / précédent | `Tab` / `Shift-Tab` |
| Enregistrer (après 400 ms sans frappe) | `↵` dans un champ d'une ligne ou une liste, `Ctrl-S` / `Ctrl-↵` (macOS : `⌘S`, `⌘↵`) |
| Mot de passe (modification) : garder, changer, oublier (« Keep » et « Forget » inactifs quand aucun n'est mémorisé) | boutons « Keep », « Change », « Forget » |
| Mot de passe sans vault (ou vault verrouillé) : créer ou déverrouiller le vault, puis l'hôte est enregistré ; « Not now » revient au formulaire | `↵` / `Esc` |
| Annuler ; avec des modifications, « Discard your changes? » | `Esc`, puis `y` (abandonner) ou `n` / `Esc` (continuer) |
| Lecture seule (hôte de `~/.ssh/config` ou importé ; aussi « Delete host » sur ces hôtes) : dupliquer / éditer tags et notes | `D` ou `↵` / `t` |
| Dupliquer : nouvel alias | `↵` |
| Supprimer un hôte Portico : confirmer / garder | `y` / `n` |

## Import d'hôtes (GUI)

« Import hosts… » (palette, `⌘⇧M` / `Ctrl-Shift-M`) : export Termius ou Termix, format reconnu par le contenu, choisi dans le sélecteur natif. Aucune clé privée n'est cochée par défaut. Voir `docs/IMPORT.md`.

| Action | Touches |
|---|---|
| Préfixer les alias (`termius-`, `termix-`) on/off | `p` |
| Cocher / décocher la n-ième clé privée | `1` … `9`, ou `Espace` sur la case |
| Importer (après 400 ms sans frappe ; si les passphrases diffèrent, le pied l'indique et `↵` montre l'erreur) ; aucun hôte à importer : choisir un autre fichier | `↵` |
| Rapport : copier la ligne `Include` | `c` |
| Annuler / fermer | `Esc` |

## Journaux de session (GUI)

Panneau debug : sessions à gauche (vivantes et 20 dernières terminées, sondes « probe » de copy-id comprises), à droite les journaux ou la config effective (`ssh -G`). Ouvert depuis un onglet, il présélectionne sa session. Les textes sont affichés échappés (contrôles, bidi), comme dans « Copy all ». Les mots-clés qui exécutent des commandes (`ProxyCommand`, `LocalCommand`, `RemoteCommand`, `KnownHostsCommand`) sont masqués (« Hidden: may contain a secret · never copied ») : leur valeur n'est jamais envoyée à la webview. Une option fixée par Portico à la connexion a l'origine « at connect ». Dans la liste des sessions, l'issue est un libellé court (« Failed · host key changed », code en infobulle) ; « Refused » (clé hôte refusée volontairement) est neutre.

| Action | Touches |
|---|---|
| Session précédente / suivante | `k` / `j`, `↑` / `↓` |
| Onglet Logs / Effective config | `Tab` (aussi `1` / `2`) |
| Filtrer (texte ; `Esc` vide le filtre) | `/` |
| Niveau minimal suivant (Debug → Info → Warn → Error) | `v`, ou les boutons |
| Défiler les records (une page, début, fin ; la fin relance le suivi) | `PgUp` / `PgDn`, `Home` / `g`, `End` / `G` |
| Suivre en direct on/off (défiler vers le haut l'arrête) | `f` |
| Copier tous les records (texte `to_line` ; sans presse-papiers, le texte s'affiche sélectionné) | `y` (aussi `c`) |
| Rafraîchir (et recalculer la config effective) | `Ctrl-R` (aussi `r`) |
| Fermer | `Esc` |

## Fichiers (GUI)

Même grammaire que la TUI (`r` renommer, `d` supprimer, `x` annuler, `Ctrl-R` rafraîchir, `k`/`o`/`s` en cas de conflit) ; chaque action figure aussi dans la palette avec sa touche. Panneau à droite du terminal de l'onglet actif. Il prend le clavier à l'ouverture ; un clic dans le terminal le lui rend. Les fichiers à envoyer viennent du sélecteur natif ou d'un glisser-déposer sur la fenêtre ; les téléchargements vont dans le dossier affiché en bas du panneau (« Change » ou la palette pour en choisir un autre).

| Action | Touches |
|---|---|
| Se déplacer | `j` / `k`, `↓` / `↑`, `Home` / `End` |
| Ouvrir le dossier, ou télécharger le fichier | `↵`, `→`, double-clic |
| Dossier parent | `Backspace`, `←`, fil d'Ariane |
| Envoyer des fichiers (sélecteur natif) | `u`, ou déposer des fichiers sur le panneau |
| Nouveau dossier (saisie en place, `↵` valide, `Esc` annule) | `n` |
| Renommer (saisie en place) | `r` ou `F2` |
| Supprimer (confirmation, focus sur `Keep` : `y` supprime, `n` / `Esc` garde) | `d`, `F8` ou `Suppr` |
| Annuler le dernier transfert en cours | `x` |
| Afficher / masquer les fichiers cachés | `.` |
| Rafraîchir (dans le panneau ; ailleurs `Ctrl-R` recharge `ssh_config`) | `Ctrl-R` |
| Destination existante | `k` garder les deux (copie numérotée) · `o` écraser · `s` ignorer · `Esc` annuler |
| Session pas encore prête / terminée | le panneau affiche la phase, ou l'erreur avec son hint ; `↵` reconnecte |
| Revenir au terminal (le panneau reste ouvert) | clic dans le terminal ; le raccourci Fichiers ou `Esc` le ferme |

## Tunnels (GUI)

| Action | Touches |
|---|---|
| Ajouter | saisir `-L 8080:db:5432`, `-R 9000:localhost:3000` ou `-D 1080` (proxy SOCKS5), puis `↵` |
| Choisir un tunnel ou un forward proposé | `↑` / `↓` |
| Démarrer le forward proposé choisi / arrêter le tunnel choisi | `↵` / `Suppr` (ou boutons `Start` / `Stop`) |
| Écoute exposée (hors loopback) | bloc d'avertissement, focus sur `Cancel` ; `Expose tunnel` actif après 400 ms sans frappe ni clic · `Esc` annule |
| Fermer | `Esc` |

## Clés (GUI)

Dialogue large : clés locales de `~/.ssh`, puis identités de l'agent (seules les listes défilent), puis le bandeau du vault, toujours visible (masqué quand le trousseau système est utilisé). Le premier focus est sur la liste ; chaque action a aussi son bouton (« Copy key », « Copy to host… », « Add to agent » / « Remove from agent », « New key ») et son entrée de palette, qui affiche la touche (« New key » : raccourci Clés, puis `n` ; vault : `v` / `V`).

| Action | Touches |
|---|---|
| Se déplacer | `j` / `k`, `↓` / `↑`, `Home` / `End` |
| Nouvelle clé (Ed25519 par défaut, RSA 3072/4096, ECDSA P-256/384/521 ; `↵` crée ; passphrase vide ou d’au moins 8 caractères, compteur plafonné « n / 8 min » ; après un refus, la passphrase effacée doit être ressaisie, ou son absence cochée, avant de recréer) | `n` |
| Ajouter à l'agent (durée : « Until removed », 1 h, 8 h ; passphrase demandée si la clé est chiffrée) | `a` |
| Retirer de l'agent (confirmation, focus sur `Keep` : `y` retire, `n` / `Esc` garde) | `x` |
| Copier sur un hôte (recherche d'hôte, `↑` `↓`, `↵`) | `c` |
| Copier la clé publique (presse-papiers ; s'il est indisponible, la ligne s'affiche, prête à sélectionner) | `y` |
| Vault : déverrouiller / verrouiller (ou créer s'il n'existe pas) | `v` |
| Vault : changer la passphrase (ou créer) | `V` |
| Rafraîchir (rappelé dans le titre du dialogue) | `Ctrl-R` |
| Fermer (depuis un sous-dialogue : revenir à la liste) | `Esc` |

**Copy-id** (« Copy to host ») : étapes « Connecting · Installing · Verifying ». Réutilise la session vivante de l'hôte, sinon en ouvre une (prompts habituels) et la ferme ensuite. Un prompt lié à la copie (clé hôte, mot de passe, passphrase de la vérification) porte le sous-titre « Copy <clé> to <hôte> · <étape> ». Pendant la copie, `Esc` (« Hide ») masque le dialogue sans l'interrompre : une notification dit comment elle s'est terminée. Résultat : `↵` termine, `c` copie les lignes `Host` / `IdentityFile` à ajouter soi-même (Portico n'écrit jamais `~/.ssh/config`). Échec : message et hint, `r` ou `↵` réessaie, `Esc` ferme.

**Vault au démarrage** : verrouillé, Portico demande la passphrase maître une fois ; `Esc` (« Continue without ») laisse les secrets non mémorisés jusqu'au déverrouillage (`v` dans Clés). Cocher « Remember » sans vault (ou vault verrouillé) propose ensuite de le créer (ou de le déverrouiller).

## Snippets (GUI)

| Action | Touches |
|---|---|
| Exécuter depuis la palette (une entrée « Run: <nom> » par snippet de l'hôte de l'onglet actif) | palette |
| Se déplacer | `j` / `k`, `↓` / `↑`, `Home` / `End` |
| Filtrer (dans le filtre : `↑` `↓`, `↵` exécute, `Esc` vide puis rend la liste) | `/` |
| Exécuter (« Run ») ou coller (« Paste ») dans l'onglet actif (formulaire des placeholders si besoin, `↵` envoie) | `↵`, double-clic |
| Nouveau / éditer / supprimer (aussi depuis le filtre) | `n` / `e` / `d`, ou `Ctrl-N` / `Ctrl-E` / `Ctrl-D` |
| Enregistrer dans l'éditeur (`↵` dans un champ d'une ligne aussi) | `Ctrl-S` (macOS : `⌘S`), `Ctrl-↵` (`⌘↵`) |
| Confirmer la suppression (focus sur `Keep` : `y` supprime, `n` / `Esc` garde) | `y` |
| Snippet multi-ligne « paste only » refusé sans bracketed paste (garde-fou, encadré d'avertissement) | `r` « Run it » (exécute chaque ligne, Entrée envoyée), après réarmement (400 ms sans frappe) ; `↵` ne fait rien ; `Esc` annule |
| Fermer | `Esc` |

Sans terminal ouvert, la liste reste éditable et l'exécution est désactivée ; la bannière « No open terminal » donne l'action suivante (« `Esc`, then `↵` to connect <hôte> », ou « reconnect » pour un onglet terminé). Les snippets hors portée de l'hôte sont affichés en retrait (« not for <hôte> ») et ne s'exécutent pas.

## Enregistrements (GUI)

« Record session » (`session.record`) sur le panneau actif ouvre la confirmation « Record this terminal? » (« Record this pane? » dans un onglet divisé) ; « Recordings… » (`v` depuis la liste des hôtes, `hosts.recordings`, ou la palette, `recordings.open`) ouvre la liste.

| Action | Touches |
|---|---|
| Confirmer l'enregistrement (seul `y`, actif après 400 ms sans frappe ; `↵` ne fait rien) / annuler | `y` / `Esc` ou `n` |
| Arrêter l'enregistrement du panneau actif (sans confirmation ; « Recording saved · <fichier> ») | `session.record` à nouveau, ou « Stop recording » dans la palette |
| Liste : se déplacer | `j` / `k`, `↓` / `↑`, `Home` / `End` |
| Liste : lire (fichier valide) / supprimer (confirmation, focus sur `Keep` : `y` supprime, `n` / `Esc` garde ; refusé d'emblée pendant l'écriture) | `↵` ou double-clic / `Suppr` ou `x` |
| Liste : rafraîchir | `Ctrl-R` |
| Lecture : pause / reprise (à la fin : rejouer depuis le début) | `Espace` |
| Lecture : vitesse ×0,5 · ×1 · ×2 · ×4 · ×8 | `+` / `-` |
| Lecture : sauter le silence en cours / reculer de 5 s / revenir au début (aussi les boutons « ↺ 5 s » et « Restart ») | `→` / `←` / `Home` |
| Lecture : aller à un instant | clic sur la barre de progression |
| Fermer la lecture (retour à la liste) / fermer la liste | `q` ou `Esc` / `Esc` |
<!-- END GENERATED: gui -->
