# Raccourcis clavier : Portico

> Source de vérité : `apps/portico-tui/src/keymap.rs` (TUI, aide `?` générée depuis `HELP`) et
> `apps/portico-gui/src/lib/shortcuts.ts` (GUI, sections GUI générées : `pnpm gen:keybindings`).
> Toute modification de raccourci met à jour ce fichier.
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
| Explorateur de fichiers (SFTP) de l'hôte (se connecte au besoin) | `f` | `f` |
| Tunnels | `p` | `p` |
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
| Explorateur de fichiers (SFTP) | `Ctrl-Space` puis `f` |
| Tunnels | `Ctrl-Space` puis `p` |
| Clés (locales, agent, vault) | `Ctrl-Space` puis `i` (depuis la liste : `i`) |
| Snippets de l'hôte de l'onglet | `Ctrl-Space` puis `s` (depuis la liste : `s`) |
| Rechercher un hôte | `Ctrl-Space` puis `/` |
| Aide | `Ctrl-Space` puis `?` |
| Envoyer `Ctrl-Space` au programme distant | `Ctrl-Space` deux fois |
| Remonter / descendre dans l'historique | `Shift-PgUp` / `Shift-PgDn` (toute frappe revient en bas) |
| Coller | collage du terminal hôte (bracketed paste si le programme distant l'active) |

## Explorateur de fichiers (TUI)

Deux panneaux, local à gauche et distant à droite, sur la session de l'onglet actif (ou de l'hôte sélectionné).
Le panneau actif a la bordure d'accent. La file des transferts s'affiche en bas tant qu'elle n'est pas vide.

| Action | Touches |
|---|---|
| Changer de panneau | `Tab` / `Shift-Tab` |
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

## Tunnels (TUI)

Panneau superposé : tunnels actifs de toutes les sessions, puis ceux déclarés dans `ssh_config`
(`LocalForward`, `RemoteForward`) pour l'hôte courant, proposés mais jamais démarrés d'office.

| Action | Touches |
|---|---|
| Ajouter (`-L 8080:db:5432`, `-R 9000:localhost:3000`) | `a` (ou `n`), saisie, `↵` |
| Démarrer un tunnel proposé | `↵` |
| Arrêter le tunnel sélectionné | `x`, `d` ou `Suppr` |
| Confirmer une écoute exposée (adresse autre que loopback) | `y` (toute autre touche annule) |
| Fermer | `Esc`, `q`, `Ctrl-C` |

## Clés (TUI)

`i` depuis la liste des hôtes, `Ctrl-Space i` partout. Panneau en deux sections : clés locales
(`~/.ssh`), puis identités de l'agent ; statut du vault en pied (seulement si le keyring système
n'est pas disponible). Seules les touches utilisables sur la sélection sont proposées.

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

Quand la liste dépasse la hauteur, elle défile avec la sélection (l'en-tête de section reste visible
sur sa première ligne) et « ↑ n more / ↓ n more » compte les clés masquées. Pendant une question en
ligne (durée, retrait), la ligne sélectionnée garde son fond. Une notice qui ne tient pas sous la liste
s'affiche en toast sous le panneau.

Formulaire de génération : `Tab` / `Shift-Tab` (ou `↓` / `↑`) entre les champs, `←` `→` ou `Espace`
sur le type, `Ctrl-U` vide le champ, `↵` génère (les erreurs s'affichent sous le champ), `Esc` annule.
La passphrase fait au moins 8 caractères, ou est vide (compteur « n / 8 min »). Si le moteur refuse la
génération alors qu'une passphrase avait été envoyée, les champs sont effacés : `↵` reste bloqué tant
qu'elle n'est pas ressaisie, ou que `Ctrl-N` ne confirme « sans passphrase ».

## Snippets (TUI)

`s` depuis la liste des hôtes, `Ctrl-Space s` partout. Liste des snippets qui s'appliquent à l'hôte
de l'onglet actif (portée affichée : `global`, `host web`, `#tag`). La frappe filtre la liste, d'où
les commandes en `Ctrl`. Sans onglet terminal ouvert sur l'hôte, l'exécution est désactivée (le
panneau l'explique) ; la gestion reste possible.

| Action | Touches |
|---|---|
| Filtrer / vider le filtre | saisie / `Ctrl-U` (ou `Esc` si le filtre n'est pas vide) |
| Se déplacer | `↓` / `↑`, `PgUp` / `PgDn` |
| Exécuter (« run » : tapé puis Entrée) ou coller (« paste » : tapé seulement) dans l'onglet ; formulaire si `{{placeholders}}` | `↵` |
| Nouveau / éditer | `Ctrl-N` / `Ctrl-E` |
| Supprimer (confirmation) | `Ctrl-D` (ou `Suppr`), puis `y` |
| Recharger | `Ctrl-R` |
| Fermer | `Esc`, `Ctrl-C` |

Sans terminal ouvert, le panneau l'indique (« No open terminal on <hôte> ») avec l'étape suivante :
`Esc`, puis `↵` pour se connecter.

Formulaire des placeholders : un champ par `{{nom}}`, `Tab` entre les champs, aperçu de ce qui sera
tapé, `↵` exécute ou colle (erreurs en ligne), `Esc` revient. Éditeur : `Tab` entre les champs, `↵`
dans la commande insère une ligne, `←` `→` change la portée, `Espace` bascule « Run » (« yes — Enter is
sent » / « no — typed only »), `Ctrl-S` (ou `↵` hors commande) enregistre, `Esc` annule. Un snippet
multi-ligne « paste only » refusé (le programme distant n'accepte pas le collage groupé) demande
confirmation : seul `r` l'exécute quand même (touches ignorées pendant 400 ms, `↵` ne fait rien),
`Esc` annule.

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
- Une entrée remplace toutes les combinaisons par défaut de l'action. Les actions globales exigent `Mod`,
  `Ctrl`, `Alt` ou une touche `F` ; les actions de la liste des hôtes acceptent une touche seule.
- Entrées ignorées (le défaut reste) et listées dans une notification au démarrage : action inconnue,
  combinaison illisible, conflit avec une autre action, combinaison réservée (copier/coller du terminal
  `Ctrl-Shift-C` / `Ctrl-Shift-V` / `Shift-Insert`, menu Édition et `⌘Q` / `⌘H` / `⌘M` sur macOS,
  `Ctrl-<touche>` seul dans un terminal, `Esc` et `Tab`), action fixe (`tab.copy`, `tab.paste`).
- Un `settings.toml` invalide (au sens du moteur) est ignoré entièrement et signalé ; Portico ne le réécrit
  pas. La palette, les infobulles et la barre de statut affichent les combinaisons effectives.
- Le thème est appliqué avant le premier rendu (dernier thème mémorisé localement, confirmé par
  `settings.toml`) ; un nom inconnu donne le thème par défaut et une notification.

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
| Favori on/off (hôte sélectionné, ou celui de l'onglet) | `app.favorite` | `⌘D` | `Ctrl-Shift-D` |
| Nouvel onglet sur le même hôte (depuis la liste : se connecter) | `tab.new` | `⌘T` | `Ctrl-Shift-T` |
| Fermer l'onglet (le dernier onglet d'un hôte le déconnecte ; pendant la connexion : annuler) | `tab.close` | `⌘W` | `Ctrl-Shift-W` |
| Onglet suivant | `tab.next` | `Ctrl-Tab`, `⌘]` | `Ctrl-Tab` |
| Onglet précédent | `tab.prev` | `Ctrl-Shift-Tab`, `⌘[` | `Ctrl-Shift-Tab` |
| Liste des hôtes | `tab.hosts` | `⌘0` | `Alt-0` |
| Aller à l'onglet n (`tab.goto-1` … `tab.goto-9`) | `tab.goto-1` | `⌘1` | `Alt-1` |
| Copier la sélection du terminal (macOS : menu Édition, `⌘C`) | `tab.copy` (fixe) | — | `Ctrl-Shift-C` |
| Coller (macOS : `⌘V` ; Linux aussi `Shift-Insert`, clic milieu) | `tab.paste` (fixe) | — | `Ctrl-Shift-V` |
| Reconnecter l'hôte de l'onglet | `tab.reconnect` | — | — |
| Déconnecter l'hôte de l'onglet | `tab.disconnect` | — | — |
| Vider « Recent » (confirmation) | `hosts.clear-recent` | — | — |
| Nouvelle clé | `keys.new` | — | — |
| Nouveau snippet | `snippets.new` | — | — |
| Changer de dossier de téléchargement | `files.download-dir` | — | — |

## Liste des hôtes (GUI)

Actives quand la liste des hôtes est affichée et que le focus n'est pas dans un champ (la recherche garde ses touches).

| Action | Identifiant | macOS | Linux |
|---|---|---|---|
| Recharger `~/.ssh/config` et `hosts.toml` | `app.reload` | `⌘R` | `Ctrl-R` |
| Rechercher (flou, `#tag`, `*` favoris, `@groupe`) | `hosts.search` | `/` | `/` |
| Hôte suivant | `hosts.down` | `j`, `↓` | `j`, `↓` |
| Hôte précédent | `hosts.up` | `k`, `↑` | `k`, `↑` |
| Se connecter (ouvre un onglet terminal, ou y revient ; aussi double-clic) | `hosts.connect` | `↵` | `↵` |
| Éditer tags, groupe et notes de l'hôte sélectionné (depuis un onglet : palette, « Edit <alias> ») | `hosts.edit` | `e` | `e` |
| Se déconnecter (hôte sélectionné) | `hosts.disconnect` | `d` | `d` |
| Fichiers (SFTP) de l'hôte sélectionné (se connecte au besoin) | `hosts.files` | `f` | `f` |
| Tunnels | `hosts.tunnels` | `p` | `p` |
| Clés | `hosts.keys` | `i` | `i` |
| Snippets de l'hôte sélectionné | `hosts.snippets` | `s` | `s` |
| Retirer de « Recent » (sur une ligne de la section Recent) | `hosts.forget-recent` | `x` | `x` |
| Replier le groupe de l'hôte sélectionné | `hosts.collapse` | `←` | `←` |
| Déplier le groupe | `hosts.expand` | `→` | `→` |
| Thème suivant (mémorisé dans `settings.toml`) | `app.theme` | `t` | `t` |

## Terminal (GUI) : touches fixes

Toutes les touches partent vers l'hôte distant, sauf les raccourcis de l'app (tableau « Application et onglets »).

| Action | Touches |
|---|---|
| Reconnecter un onglet terminé ou en échec | `↵` |
| Annuler une connexion en cours / revenir aux hôtes (onglet sans shell) | `Esc` |
| Barre d'onglets (focus clavier) : onglet voisin, premier / dernier, fermer | `←` `→`, `Home` / `End`, `Suppr` |
| Fermer un onglet (souris) | clic milieu sur l'onglet |
| Coller (Linux) | `Ctrl-Shift-V`, `Shift-Insert`, clic milieu |

Tout collage est assaini (ESC et contrôles C1 retirés) avant d'atteindre le terminal.

## Liste des hôtes (GUI) : sections

Sans recherche, la liste montre « Recent » (les 5 derniers hôtes encore présents dans la config), puis un titre par groupe (ordre alphabétique), puis « Other » ; sans groupe ni récent, une liste plate. Avec une recherche, les résultats classés, à plat. Les titres de section sont des lignes : on s'y déplace comme sur un hôte.

| Action | Touches |
|---|---|
| Plier / déplier la section (sur son titre) | `↵`, clic |
| Retirer de « Recent » (souris) | `×` au survol de la ligne |
| Naviguer pendant la recherche | `↑` `↓`, `Ctrl-N` / `Ctrl-P` |
| Vider la recherche | `Esc` |
| Favori (souris) | étoile à côté du nom, dans les détails |

## Édition d'un hôte (GUI)

« Edit <alias> » : tags, groupe et notes, enregistrés dans `hosts.toml` par le moteur (Portico n'écrit jamais `~/.ssh/config`). Les erreurs s'affichent sous le champ concerné, avec un conseil.

| Action | Touches |
|---|---|
| Ajouter un tag (`#` facultatif) | `Espace`, `,` ou `↵` dans le champ Tags |
| Retirer le dernier tag | `Backspace` dans le champ vide |
| Groupe : choisir une suggestion (groupes existants) | `↓` / `↑`, puis `↵` |
| Champ suivant | `Tab` |
| Enregistrer | `Ctrl-S` (macOS : `⌘S`), `Ctrl-↵`, ou `↵` dans un champ d'une ligne |
| Annuler | `Esc` |

## Journaux de session (GUI)

Panneau debug : sessions à gauche (vivantes et 20 dernières terminées, sondes « probe » de copy-id comprises), à droite les journaux ou la config effective (`ssh -G`). Ouvert depuis un onglet, il présélectionne sa session. Les textes sont affichés échappés (contrôles, bidi), comme dans « Copy all ». Les mots-clés qui exécutent des commandes (`ProxyCommand`, `LocalCommand`, `RemoteCommand`, `KnownHostsCommand`) sont masqués et jamais copiés.

| Action | Touches |
|---|---|
| Session précédente / suivante | `k` / `j`, `↑` / `↓` |
| Onglet Logs / Effective config | `1` / `2` |
| Filtrer (texte ; `Esc` vide le filtre) | `/` |
| Niveau minimal | boutons Debug, Info, Warn, Error |
| Suivre en direct on/off (défiler vers le haut l'arrête) | `f` |
| Copier tout le journal (texte `to_line` ; sans presse-papiers, le texte s'affiche sélectionné) | `c` |
| Rafraîchir (et recalculer la config effective) | `r` |
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
| Ajouter | saisir `-L 8080:db:5432` ou `-R 9000:localhost:3000`, puis `↵` |
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
<!-- END GENERATED: gui -->

## Prévus

| Action | TUI | GUI | Phase |
|---|---|---|---|
| Logs / debug de la session | `Ctrl-Space l` | `⌘⇧L` | 4 |
| Favori on/off | `*` (`f` ouvre l'explorateur) | `⌘D` | 4 |
