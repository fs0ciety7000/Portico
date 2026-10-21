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

## Terminal (GUI)

Toutes les touches partent vers l'hôte distant, sauf les raccourcis de l'app ci-dessous.

| Action | macOS | Linux |
|---|---|---|
| Nouvel onglet sur le même hôte (depuis la liste : se connecter) | `⌘T` | `Ctrl-Shift-T` |
| Fermer l'onglet (le dernier onglet d'un hôte le déconnecte ; pendant la connexion : annuler) | `⌘W` | `Ctrl-Shift-W` |
| Onglet n / liste des hôtes | `⌘1`…`⌘9` / `⌘0` | `Alt-1`…`Alt-9` / `Alt-0` |
| Onglet suivant / précédent | `Ctrl-Tab` / `Ctrl-Shift-Tab`, `⌘]` / `⌘[` | `Ctrl-Tab` / `Ctrl-Shift-Tab` |
| Palette de commandes | `⌘K` | `Ctrl-Shift-K` |
| Fichiers (SFTP) de cet hôte, à côté du terminal | `⌘⇧F` | `Ctrl-Shift-F` |
| Tunnels | `⌘⇧P` | `Ctrl-Shift-P` |
| Clés | `⌘⇧I` | `Ctrl-Shift-I` |
| Snippets de cet hôte (la palette liste aussi « Run: <nom> » par snippet applicable) | `⌘⇧S` | `Ctrl-Shift-S` |
| Copier la sélection / coller | `⌘C` / `⌘V` | `Ctrl-Shift-C` / `Ctrl-Shift-V`, `Shift-Insert`, clic milieu |
| Reconnecter un onglet terminé ou en échec | `↵` | `↵` |
| Annuler une connexion en cours / revenir aux hôtes (onglet sans shell) | `Esc` | `Esc` |
| Barre d'onglets (focus clavier) : onglet voisin, premier / dernier, fermer | `←` `→`, `Home` / `End`, `Suppr` | idem |
| Fermer un onglet (souris) | clic milieu sur l'onglet | clic milieu sur l'onglet |

Tout collage est assaini (ESC et contrôles C1 retirés) avant d'atteindre le terminal.

## Fichiers (GUI)

Même grammaire que la TUI (`r` renommer, `d` supprimer, `x` annuler, `Ctrl-R` rafraîchir, `k`/`o`/`s` en cas de conflit) ;
chaque action figure aussi dans la palette avec sa touche.

Panneau à droite du terminal de l'onglet actif. Il prend le clavier à l'ouverture ; un clic dans le terminal le lui rend.
Les fichiers à envoyer viennent du sélecteur natif ou d'un glisser-déposer sur la fenêtre ; les téléchargements vont dans
le dossier affiché en bas du panneau (« Change » ou la palette pour en choisir un autre).

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
| Revenir au terminal (le panneau reste ouvert) | clic dans le terminal ; `⌘⇧F` / `Ctrl-Shift-F` ou `Esc` le ferme |

## Tunnels (GUI)

| Action | Touches |
|---|---|
| Ajouter | saisir `-L 8080:db:5432` ou `-R 9000:localhost:3000`, puis `↵` |
| Choisir un tunnel ou un forward proposé | `↑` / `↓` |
| Démarrer le forward proposé choisi / arrêter le tunnel choisi | `↵` / `Suppr` (ou boutons `Start` / `Stop`) |
| Écoute exposée (hors loopback) | bloc d'avertissement, focus sur `Cancel` ; `Expose tunnel` actif après 400 ms sans frappe ni clic · `Esc` annule |
| Fermer | `Esc` |

## Clés (GUI)

Dialogue large : clés locales de `~/.ssh`, puis identités de l'agent, puis le vault (masqué quand le trousseau
système est utilisé). Le premier focus est sur la liste ; chaque action a aussi son bouton et son entrée de palette.

| Action | Touches |
|---|---|
| Ouvrir (depuis la liste des hôtes / partout ; aussi « Keys » dans la palette) | `i` / `⌘⇧I` (Linux : `Ctrl-Shift-I`) |
| Se déplacer | `j` / `k`, `↓` / `↑`, `Home` / `End` |
| Nouvelle clé (Ed25519 par défaut, RSA 3072/4096, ECDSA P-256/384/521 ; `↵` crée) | `n` |
| Ajouter à l'agent (durée : jusqu'au retrait, 1 h, 8 h ; passphrase demandée si la clé est chiffrée) | `a` |
| Retirer de l'agent (confirmation, focus sur `Keep` : `y` retire, `n` / `Esc` garde) | `x` |
| Copier sur un hôte (recherche d'hôte, `↑` `↓`, `↵`) | `c` |
| Copier la clé publique (presse-papiers) | `y` |
| Vault : déverrouiller / verrouiller (ou créer s'il n'existe pas) | `v` |
| Vault : changer la passphrase (ou créer) | `V` |
| Rafraîchir | `Ctrl-R` |
| Fermer (depuis un sous-dialogue : revenir à la liste) | `Esc` |

**Copy-id** : étapes « Connecting · Installing · Verifying ». Réutilise la session vivante de l'hôte, sinon en ouvre
une (prompts habituels) et la ferme ensuite. Résultat : `↵` termine, `c` copie les lignes `Host` / `IdentityFile`
à ajouter soi-même (Portico n'écrit jamais `~/.ssh/config`). Échec : message et hint, `↵` réessaie, `Esc` ferme.

**Vault au démarrage** : verrouillé, Portico demande la passphrase maître une fois ; `Esc` (« Continue without ») laisse
les secrets non mémorisés jusqu'au déverrouillage (`v` dans Clés). Cocher « Remember » sans vault (ou vault verrouillé)
propose ensuite de le créer (ou de le déverrouiller).

## Snippets (GUI)

| Action | Touches |
|---|---|
| Ouvrir (hôte sélectionné depuis la liste / onglet actif ; aussi « Snippets » dans la palette) | `s` / `⌘⇧S` (Linux : `Ctrl-Shift-S`) |
| Exécuter depuis la palette (une entrée « Run: <nom> » par snippet de l'hôte de l'onglet actif) | `⌘K` (Linux : `Ctrl-Shift-K`) |
| Se déplacer | `j` / `k`, `↓` / `↑`, `Home` / `End` |
| Filtrer (dans le filtre : `↑` `↓`, `↵` exécute, `Esc` vide puis rend la liste) | `/` |
| Exécuter dans l'onglet actif (formulaire des placeholders si besoin, `↵` envoie) | `↵`, double-clic |
| Nouveau / éditer (`⌘↵` ou `Ctrl-↵` enregistre ; `↵` dans un champ d'une ligne aussi) | `n` / `e` |
| Supprimer (confirmation, focus sur `Keep` : `y` supprime, `n` / `Esc` garde) | `d` |
| Snippet multi-ligne « paste only » refusé sans bracketed paste | `↵` « Run it » (l'exécute), `Esc` annule |
| Fermer | `Esc` |

Sans terminal ouvert, la liste reste éditable et l'exécution est désactivée (bannière explicative).
Les snippets hors portée de l'hôte sont affichés en retrait (« not for <hôte> ») et ne s'exécutent pas.

## Prévus

| Action | TUI | GUI | Phase |
|---|---|---|---|
| Logs / debug de la session | `Ctrl-Space l` | `⌘⇧L` | 4 |
| Favori on/off | `*` (`f` ouvre l'explorateur) | `⌘D` | 4 |
