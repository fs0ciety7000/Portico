//! Generates the TUI part of `docs/KEYBINDINGS.md` from the keymap.
//!
//! The block between [`BEGIN`] and [`END`] is produced by [`block`] from
//! [`crate::keymap::BINDINGS`] (default keys and identifiers) and the fixed
//! panel tables below; everything outside the block is free text and is
//! kept by [`write`]. A test fails when the checked-in file is stale
//! (regenerate with `cargo run -p portico-tui -- --write-keybindings
//! docs/KEYBINDINGS.md`).

use std::fmt::Write as _;
use std::path::Path;

use crate::keymap::{BINDINGS, Context, Keymap};

pub(crate) const BEGIN: &str = "<!-- BEGIN GENERATED: tui -->";
pub(crate) const END: &str = "<!-- END GENERATED: tui -->";

/// A fixed table: intro paragraph, then `(action, keys)` rows (Markdown).
struct Panel {
    title: &'static str,
    intro: &'static str,
    rows: &'static [(&'static str, &'static str)],
    outro: &'static str,
}

const SEARCH: Panel = Panel {
    title: "TUI : recherche",
    intro: "Le champ de recherche (`/`) prend la saisie : ses touches sont fixes. `#tag`, `*` (favoris) et `@groupe` filtrent ; la liste devient plate et classée.",
    rows: &[
        (
            "Naviguer pendant la recherche",
            "`↑` `↓`, `Ctrl-N` / `Ctrl-P`",
        ),
        ("Se connecter à l'hôte sélectionné", "`↵`"),
        ("Effacer un caractère / tout", "`Backspace` / `Ctrl-U`"),
        ("Quitter le champ (le filtre reste)", "`Esc`"),
    ],
    outro: "",
};

const HOST_FORM: Panel = Panel {
    title: "TUI : hôtes créés dans Portico",
    intro: "« Add host… » (`a` ou `n`), « Edit host… » (`e`, hôte Portico), « Duplicate as Portico host… » (`D`) et « Delete host » (`Del`) depuis la liste. Ces hôtes vivent dans le fragment `hosts.conf` de Portico ; Portico n'écrit jamais `~/.ssh/config`. Titres : « Add host », « Edit <alias> », « Duplicate <alias> » (sans points de suspension). Champs : alias, `HostName`, `User`, `Port`, clé (choisie parmi les clés locales découvertes, ou « none · agent and default keys »), `ProxyJump` (un hôte existant, ou « none »), mot de passe facultatif juste après (masqué, mémorisé dans le trousseau ou le vault), tags, groupe, favori, notes. À l'édition, le formulaire dit « A password is remembered » (`Keep`, `Change` ou `Forget`) ou « No password remembered » (seul « Set », champ vide). Sous un mot de passe saisi ou gardé, une note dit où il part : « Sent automatically to the server that answers at this address, after its host key is verified; then tied to that key. » ; si `HostName`, `User` ou `Port` changent, elle devient « Changing the server or user forgets the remembered password. Choose Change to set one for the new destination. » Sans vault (ou vault verrouillé), enregistrer un mot de passe ouvre « Create the vault » (ou « Unlock the vault ») puis enregistre de nouveau ; `Esc` (« not now ») revient au formulaire, le mot de passe toujours saisi et le refus sous le champ. Chaque valeur est vérifiée par le moteur : un refus s'affiche sous le champ concerné (choisi par son code d'erreur), avec son hint (sauf pour le port, dont le message suffit). Un hôte Portico issu d'un import l'indique (« Portico · from Termix » dans les détails et le titre) et son formulaire précise : « Editing it keeps later imports from updating it. » Un hôte de `~/.ssh/config` s'ouvre en lecture seule : « Defined in ~/.ssh/config, which Portico never edits. Duplicate it as a Portico host to change it. » ; un hôte de l'ancien fichier d'import : « Imported by an earlier version of Portico (imports/termius.conf), which it never edits. Duplicate it as a Portico host to change it. » (`D` duplicate, `t` tags and notes).",
    rows: &[
        (
            "Champ suivant / précédent",
            "`Tab` / `Shift-Tab`, `↓` / `↑`",
        ),
        (
            "Choisir (clé, `ProxyJump`, favori, mot de passe)",
            "`←` / `→`, `Space` ; une lettre saute au choix suivant qui commence par elle",
        ),
        ("Nouvelle ligne dans les notes", "`Alt-↵`"),
        ("Enregistrer", "`↵` (ou `Ctrl-S`)"),
        ("Vider le champ", "`Ctrl-U`"),
        (
            "Annuler (avec des changements : `y` les abandonne, `n` ou `Esc` revient au formulaire, les autres touches sont ignorées)",
            "`Esc`, `Ctrl-C`",
        ),
        (
            "Supprimer (« Delete <alias> (user@host:port) and its tags and notes. Its remembered password is forgotten unless another host uses the same account. Open sessions stay connected. This cannot be undone. » ; confirmation armée : touches ignorées 400 ms, puis seul `y` supprime ; `n` ou `Esc` annule, les autres touches sont ignorées). Sur un hôte non Portico, un toast « <alias> is read-only » (casse de l'alias conservée)",
            "`Del` depuis la liste",
        ),
    ],
    outro: "Après un enregistrement, la liste se recharge (`HostsChanged`) et sélectionne l'hôte. Les hôtes Portico portent `◆` dans la liste, les hôtes importés `◇` ; les détails indiquent l'origine. Tant que `~/.ssh/config` n'inclut pas `hosts.conf`, un bandeau sous les détails propose la ligne `Include` (« Optional: for ssh, scp and git to see these hosts too, add this line at the top of ~/.ssh/config: » ; `y` la copie, `z` masque le bandeau jusqu'au prochain lancement).",
};

const EDIT: Panel = Panel {
    title: "TUI : tags, groupe et notes d'un hôte en lecture seule",
    intro: "« Edit <alias> » (`e` puis `t` sur un hôte de `~/.ssh/config` ou importé). Tags, groupe et notes vivent dans `hosts.toml` ; Portico n'écrit jamais `~/.ssh/config`. Les tags se tapent librement, séparés par des espaces ou des virgules, `#` facultatif, et s'affichent en chips ; ils sont validés dès la saisie avec les règles du moteur : le tag refusé est marqué en couleur d'erreur et la raison s'affiche sous le champ, avec son hint.",
    rows: &[
        (
            "Champ suivant / précédent",
            "`Tab` / `Shift-Tab` (`↓` / `↑` hors notes)",
        ),
        ("Nouvelle ligne dans les notes", "`↵`"),
        ("Enregistrer", "`Ctrl-S` (ou `↵` hors notes)"),
        ("Vider le champ", "`Ctrl-U`"),
        ("Annuler", "`Esc`, `Ctrl-C`"),
    ],
    outro: "Les notes s'affichent dans le panneau de détails de l'hôte.",
};

const DEBUG: Panel = Panel {
    title: "TUI : journaux de session (panneau debug)",
    intro: "« Session logs » : `l` depuis la liste, `Ctrl-Space l` partout (depuis un onglet terminal, sa session est présélectionnée). À gauche, les sessions récentes (vivantes, puis les 20 dernières terminées), issue colorée, sondes de copy-id marquées « probe » ; sous 100 colonnes, cette colonne devient un sélecteur d'une ligne (« ◂ lab · 22:23 (1/3) ▸ »). À droite, deux onglets : **Logs** (enregistrements : niveau, cible, message ; suivi en direct ; la vue commence toujours sur un début d'enregistrement) et **Effective config** (« Config » si la largeur manque ; config effective de l'alias, comme `ssh -G` : route « You → bastion → hôte », chaque option avec son origine `fichier:ligne`, « default » ou « at connect », avertissements). Couleurs : WARN et ERROR en couleur de statut, INFO secondaire, DEBUG / TRACE atténués.",
    rows: &[
        (
            "Session précédente / suivante",
            "`↑` / `↓`, `k` / `j`, `[` / `]`, `←` / `→`",
        ),
        ("Onglet Logs / Effective config", "`Tab`"),
        (
            "Filtrer les enregistrements (texte)",
            "`/`, saisie, `↵` ; `Esc` vide le filtre",
        ),
        ("Niveau minimal (debug → info → warn → error)", "`v`"),
        (
            "Copier (OSC 52, texte `to_line` ; l'onglet Effective config ne copie jamais les options sensibles)",
            "`y`",
        ),
        (
            "Révéler / masquer les options sensibles (`ProxyCommand`, `LocalCommand`, `RemoteCommand`, `KnownHostsCommand` : « ••• hidden · s reveals · never copied », révélées sur cet écran seulement, jamais copiées)",
            "`s` (onglet Effective config)",
        ),
        (
            "Défiler / plus ancien / plus récent (`G` reprend le suivi)",
            "`PgUp` / `PgDn`, `g` / `G`",
        ),
        ("Rafraîchir", "`Ctrl-R`"),
        ("Aide (par-dessus le panneau)", "`?`"),
        ("Fermer", "`Esc`, `q`, `Ctrl-C`"),
    ],
    outro: "Le panneau de détails d'un hôte masque aussi ces options (« ••• hidden · l reveals in logs »). Quand une connexion échoue, ses détails proposent `↵ retry` et `l logs`.",
};

const FILES: Panel = Panel {
    title: "TUI : explorateur de fichiers",
    intro: "Deux panneaux, local à gauche et distant à droite, sur la session de l'onglet actif (ou de l'hôte sélectionné). Le panneau actif a la bordure d'accent. La file des transferts s'affiche en bas tant qu'elle n'est pas vide.",
    rows: &[
        (
            "« Switch side (local/remote) » : changer de côté",
            "`Tab` / `Shift-Tab`",
        ),
        (
            "Se déplacer",
            "`j` / `k`, `↓` / `↑`, `PgUp` / `PgDn`, `g` / `G`",
        ),
        (
            "Ouvrir le dossier / remonter",
            "`↵`, `→`, `l` / `Backspace`, `←`, `h`",
        ),
        (
            "Copier la sélection vers l'autre panneau (download ou upload)",
            "`c` ou `F5`",
        ),
        ("Nouveau dossier", "`n` ou `F7`"),
        ("Renommer", "`r` ou `F2`"),
        ("Supprimer (confirmation `y` / `n`)", "`d`, `F8` ou `Suppr`"),
        ("Annuler le dernier transfert en cours", "`x`"),
        ("Afficher / masquer les fichiers cachés", "`.`"),
        ("Rafraîchir les deux panneaux", "`Ctrl-R`"),
        (
            "Destination existante",
            "`k` garder les deux (`nom (1).ext`) · `o` écraser · `s` ignorer · `Esc` annuler",
        ),
        (
            "Session distante pas prête / terminée",
            "le panneau distant affiche la phase, ou l'erreur et son hint ; `↵` reconnecte puis rouvre SFTP",
        ),
        ("Aide", "`?`"),
        (
            "Revenir (terminal ou liste des hôtes)",
            "`Esc`, `q`, `Ctrl-C`",
        ),
    ],
    outro: "",
};

const TUNNELS: Panel = Panel {
    title: "TUI : tunnels",
    intro: "Panneau superposé : tunnels actifs de toutes les sessions, puis ceux déclarés dans `ssh_config` (`LocalForward`, `RemoteForward`, `DynamicForward`) pour l'hôte courant, proposés mais jamais démarrés d'office.",
    rows: &[
        (
            "Ajouter (`-L 8080:db:5432`, `-R 9000:localhost:3000`, `-D 1080` pour un proxy SOCKS5)",
            "`a` (ou `n`), saisie, `↵`",
        ),
        ("Démarrer un tunnel proposé", "`↵`"),
        ("Arrêter le tunnel sélectionné", "`x`, `d` ou `Suppr`"),
        (
            "Confirmer une écoute exposée (adresse autre que loopback)",
            "`y` (toute autre touche annule)",
        ),
        ("Fermer", "`Esc`, `q`, `Ctrl-C`"),
    ],
    outro: "",
};

const KEYS: Panel = Panel {
    title: "TUI : clés",
    intro: "Panneau en deux sections : clés locales (`~/.ssh`), puis identités de l'agent ; statut du vault en pied (seulement si le keyring système n'est pas disponible). Seules les touches utilisables sur la sélection sont proposées.",
    rows: &[
        (
            "Se déplacer / changer de section",
            "`j` / `k`, `↓` / `↑`, `g` / `G` · `Tab`",
        ),
        ("Nouvelle clé (formulaire)", "`n`"),
        (
            "Ajouter à l'agent (durée : « Until removed » / 1 h / 8 h, passphrase si besoin)",
            "`a`, `←` `→`, `↵`",
        ),
        (
            "Retirer de l'agent (confirmation)",
            "`x` (ou `Suppr`), puis `y`",
        ),
        (
            "Copier la clé sur un hôte (sélecteur flou, puis Connecting · Installing · Verifying ; sans clé privée, `.pub` seul, l'étape Verifying est sautée et le résultat le dit)",
            "`c`, saisie, `↵`",
        ),
        (
            "Réessayer une copie en échec / quitter la vue (la copie continue)",
            "`↵` ou `r` / `Esc`",
        ),
        (
            "Copier la clé publique (OSC 52, et la ligne s'affiche)",
            "`y` (`Esc` masque la ligne)",
        ),
        ("Vault : déverrouiller ou verrouiller", "`v`"),
        ("Vault : créer, ou changer la passphrase", "`V`"),
        ("Rafraîchir", "`Ctrl-R`"),
        ("Fermer", "`Esc`, `q`, `Ctrl-C`"),
    ],
    outro: "Quand la liste dépasse la hauteur, elle défile avec la sélection (l'en-tête de section reste visible sur sa première ligne) et « ↑ n more / ↓ n more » compte les clés masquées. Pendant une question en ligne (durée, retrait), la ligne sélectionnée garde son fond. Une notice qui ne tient pas sous la liste s'affiche en toast sous le panneau.\n\nFormulaire de génération : `Tab` / `Shift-Tab` (ou `↓` / `↑`) entre les champs, `←` `→` ou `Espace` sur le type, `Ctrl-U` vide le champ, `↵` génère (les erreurs s'affichent sous le champ), `Esc` annule. La passphrase fait au moins 8 caractères, ou est vide (compteur « n / 8 min »). Si le moteur refuse la génération alors qu'une passphrase avait été envoyée, les champs sont effacés : `↵` reste bloqué tant qu'elle n'est pas ressaisie, ou que `Ctrl-N` ne confirme « sans passphrase ».",
};

const SNIPPETS: Panel = Panel {
    title: "TUI : snippets",
    intro: "Liste des snippets qui s'appliquent à l'hôte de l'onglet actif (portée affichée : `global`, `host web`, `#tag`). La frappe filtre la liste, d'où les commandes en `Ctrl`. Sans onglet terminal ouvert sur l'hôte, l'exécution est désactivée (le panneau l'explique) ; la gestion reste possible.",
    rows: &[
        (
            "Filtrer / vider le filtre",
            "saisie / `Ctrl-U` (ou `Esc` si le filtre n'est pas vide)",
        ),
        ("Se déplacer", "`↓` / `↑`, `PgUp` / `PgDn`"),
        (
            "Exécuter (« run » : tapé puis Entrée) ou coller (« paste » : tapé seulement) dans l'onglet ; formulaire si `{{placeholders}}`",
            "`↵`",
        ),
        ("Nouveau / éditer", "`Ctrl-N` / `Ctrl-E`"),
        (
            "Supprimer (confirmation)",
            "`Ctrl-D` (ou `Suppr`), puis `y`",
        ),
        ("Recharger", "`Ctrl-R`"),
        ("Fermer", "`Esc`, `Ctrl-C`"),
    ],
    outro: "Sans terminal ouvert, le panneau l'indique (« No open terminal on <hôte> ») avec l'étape suivante : `Esc`, puis `↵` pour se connecter.\n\nFormulaire des placeholders : un champ par `{{nom}}`, `Tab` entre les champs, aperçu de ce qui sera tapé, `↵` exécute ou colle (erreurs en ligne), `Esc` revient. Éditeur : `Tab` entre les champs, `↵` dans la commande insère une ligne, `←` `→` change la portée, `Espace` bascule « Run » (« yes — Enter is sent » / « no — typed only »), `Ctrl-S` (ou `↵` hors commande) enregistre, `Esc` annule. Un snippet multi-ligne « paste only » refusé (le programme distant n'accepte pas le collage groupé) demande confirmation : seul `r` l'exécute quand même (touches ignorées pendant 400 ms, `↵` ne fait rien), `Esc` annule.",
};

const SCROLLBACK: Panel = Panel {
    title: "TUI : recherche dans l'historique",
    intro: "`Ctrl-Space /` dans un panneau terminal (`terminal.search`) ouvre une barre d'une ligne en bas du panneau (au-dessus de la barre de statut pour un onglet non divisé) : « Search », la requête et un compteur « 3 of 17 », numéroté depuis le bas comme dans la GUI (la correspondance la plus récente est 1 ; « N matches » si la courante a disparu sous la sortie ; « No match » en couleur d'avertissement, « 999+ » au-delà de 999, rien tant que la requête est vide). Texte littéral (`.*[` ne sont pas des métacaractères), casse intelligente : insensible à la casse sauf si la requête contient une majuscule. La recherche porte sur l'écran et tout l'historique du panneau (10 000 lignes). Toutes les correspondances visibles sont surlignées discrètement, la courante en couleur d'accent. Tant que la barre est ouverte, **aucune touche ni aucun collage n'atteint l'hôte distant**, et la sortie qui arrive ne déplace pas la vue (le compteur suit).",
    rows: &[
        (
            "Taper la requête (saut incrémental à la correspondance la plus proche en remontant depuis le bas de la vue)",
            "saisie, collage (première ligne)",
        ),
        (
            "Effacer un caractère / toute la requête",
            "`Backspace` / `Ctrl-U`",
        ),
        (
            "Correspondance plus ancienne (vers le haut, boucle)",
            "`↵`, `↑`, `Ctrl-P`",
        ),
        (
            "Correspondance plus récente (vers le bas, boucle)",
            "`Ctrl-N`, `↓` (`Shift-↵` seulement si le terminal hôte le distingue de `↵`, ce que la plupart ne font pas)",
        ),
        (
            "Fermer : efface les surlignages et revient en bas (sortie en direct)",
            "`Esc`, `Ctrl-C`",
        ),
        (
            "Commandes Portico (changer de panneau…) ; `Ctrl-Space` deux fois ne part pas vers l'hôte tant que la barre est ouverte",
            "`Ctrl-Space`, puis la touche",
        ),
    ],
    outro: "La barre reste toujours sur la dernière ligne du panneau : quand la correspondance courante est sur la dernière ligne de la vue en direct, la vue remonte d'une ligne pour l'afficher juste au-dessus de la barre (comme `less`). Chaque panneau a sa propre recherche : elle reste ouverte quand le focus passe à un autre panneau. Exécuter un snippet dans le panneau ferme sa recherche.",
};

const PANES: Panel = Panel {
    title: "TUI : panneaux",
    intro: "Un onglet terminal se divise en panneaux : 8 au plus, chacun d'au moins 20×5 cellules. Les commandes sont dans la table du préfixe (`prefix.split_*`, `prefix.pane_*`, `prefix.next_pane`, `prefix.resize_*`, `prefix.close_pane`, `prefix.zoom_pane`). Seul le panneau actif reçoit les frappes, le collage et le défilement (aucune diffusion).",
    rows: &[
        (
            "Choisir l'hôte du nouveau panneau (après `Ctrl-Space S` : liste « Split with host… », recherche vide et active, rendue ensuite)",
            "saisie pour filtrer, `↓` / `↑`, `↵` pour diviser",
        ),
        ("Annuler le choix d'hôte", "`Esc`, `Ctrl-C`"),
        (
            "Mode `PANE` : après `Ctrl-Space` + flèche ou `Shift` + flèche, les flèches (focus) et `Shift` + flèches (« Move divider ») restent actives 600 ms sans le préfixe ; toute autre touche quitte le mode et part au panneau",
            "`←` `↓` `↑` `→`, `Shift` + flèche",
        ),
        (
            "Rouvrir un panneau dont la connexion a échoué ou s'est fermée (reconnecte au besoin ; jamais après un changement de clé hôte)",
            "`↵` (dans le panneau actif ; un autre panneau en échec indique « focus to retry (Ctrl-Space ←↓↑→) »)",
        ),
    ],
    outro: "Chaque panneau a une ligne de titre : alias, point de statut, phase et latence. Le panneau actif porte la barre d'accent, les autres sont atténués. Un panneau en échec a un point plein et sa phase en couleur d'erreur, dans son titre comme dans l'onglet ; le point creux est réservé à un panneau sans shell (sorti, fermé). Le zoom affiche le panneau actif seul ; changer de panneau, diviser ou fermer y met fin, et diviser un panneau zoomé dézoome puis divise. Une division impossible est refusée sur une ligne de la barre de statut, pendant 4 s, sans couvrir aucun panneau (« This tab already has 8 panes · Close a pane first. », « Each pane needs at least 20 columns × 5 rows · Enlarge the window, or close a pane. »). Depuis un onglet divisé, `?` ouvre l'aide sur la page « Panes ».",
};

const RECORDINGS: Panel = Panel {
    title: "TUI : enregistrements et lecture",
    intro: "« Recordings » : `v` depuis la liste, `Ctrl-Space v` partout. Les fichiers asciicast v2 du dossier `recordings/`, du plus récent au plus ancien : alias, début en heure locale sur 24 h (`22 Oct 13:34`, l'année seulement si ce n'est pas l'année en cours ; suffixe « UTC » si le fuseau local n'a pas pu être lu au démarrage), durée (`m:ss` ou `h:mm:ss`), taille ; `●` en couleur d'erreur pour un fichier en cours d'écriture, `▲` en couleur d'avertissement pour un fichier invalide (non lisible, mais supprimable : « Not a valid asciicast v2 recording: it cannot be played, it can be deleted. » s'affiche quand il est sélectionné). Le nom du fichier sélectionné s'affiche sous la liste ; les touches sont dans la barre de statut.",
    rows: &[
        (
            "Se déplacer",
            "`j` / `k`, `↓` / `↑`, `PgUp` / `PgDn`, `g` / `G`",
        ),
        ("Lire l'enregistrement", "`↵`"),
        (
            "Supprimer (« The file is removed from the recordings folder. This cannot be undone. » ; confirmation armée : touches ignorées 400 ms, puis seul `y` supprime, `n` ou `Esc` garde le fichier, les autres touches sont ignorées). Un enregistrement en cours d'écriture est refusé d'emblée (« This recording is still being written » · « Stop the recording first ») et `d` n'est pas proposé sur sa ligne",
            "`d` ou `Suppr`, puis `y`",
        ),
        ("Rafraîchir", "`Ctrl-R`"),
        ("Fermer", "`Esc`, `q`, `Ctrl-C`"),
        (
            "Lecture : pause / reprise ; une fois terminée, rejouer depuis le début",
            "`Espace`",
        ),
        ("Lecture : vitesse (×0,5 à ×8)", "`+` / `-`"),
        ("Lecture : sauter le silence en cours", "`→`"),
        ("Lecture : revenir de 5 s", "`←`"),
        ("Lecture : reprendre au début", "`Home`"),
        (
            "Lecture : revenir à la liste",
            "`q`, `Esc` (`Ctrl-C` ferme tout)",
        ),
    ],
    outro: "Le lecteur occupe le corps de l'écran : une ligne de titre (titre de l'en-tête, toujours suivi du nom du fichier, car un fichier tiers peut porter n'importe quel titre ; puis l'état et la vitesse), la grille enregistrée dans un émulateur dédié, puis la progression « temps / total » (jauge en accent) et la taille, ou ce qui ne tient pas (« 2 rows hidden »). Sur un écran de 24 lignes, la ligne de titre disparaît : nom du fichier, état et vitesse passent sur la ligne de progression, et la grille gagne une ligne. `←` et `Home` rouvrent le fichier et rejouent instantanément jusqu'au point visé (l'émulateur repart de zéro). Les silences de plus de 2 s sont raccourcis. Une erreur de lecture s'affiche à la place de la jauge (message · conseil). Le statut de la session (`● alias · latence`) et `● REC` restent visibles dans la barre de statut. Aucune touche ni aucun collage ne part vers un terminal pendant la lecture.",
};

const UPDATES: Panel = Panel {
    title: "TUI : vérification des mises à jour (opt-in)",
    intro: "Au premier lancement, tant que `[updates] check` est absent de `settings.toml`, une question s'ouvre une fois sur la liste des hôtes : « Check for new versions once a day? Portico would contact github.com and send nothing else. » (dialogue plutôt que bandeau : `y` et `n` servent déjà sur la liste ; bordure discrète, « Change it later: U on the host list. » en retrait). Elle est armée : les touches des premières 400 ms sont ignorées. Sans accord, Portico ne fait aucune requête réseau.",
    rows: &[
        ("« Check daily » (`check = true`)", "`y`"),
        ("« Not now » (`check = false` : plus jamais demandé)", "`n`"),
        (
            "Plus tard (rien n'est écrit, redemandé au prochain lancement)",
            "`Esc`",
        ),
        (
            "Vue « Portico X is available » (`u`) : copier la commande de mise à jour de cette installation (Homebrew), sinon le lien de la page de version (OSC 52)",
            "`y`",
        ),
        (
            "Vue « Updates » (`U`, ou `u` quand aucune version plus récente n'est connue) : activer / désactiver la vérification quotidienne (jamais en une seule touche)",
            "`y`",
        ),
        ("Fermer la vue", "`Esc`, `n`"),
    ],
    outro: "Quand une version plus récente est connue, la barre de statut affiche « 0.3.1 available » en accent, à droite, jusqu'à la mise à jour (aucune fenêtre ne s'ouvre d'elle-même). La vue donne « You have 0.3.0. », le lien des notes de version et la marche à suivre selon l'installation (Homebrew, AppImage, paquet pacman, .deb, sinon la page de version) puis « Portico never downloads or installs anything itself. »",
};

const SYNC: Panel = Panel {
    title: "TUI : synchronisation chiffrée par dossier",
    intro: "« Sync » : `S` depuis la liste, `Ctrl-Space y` partout (la TUI n'a pas de palette : c'est l'équivalent de « Sync… »). Le panneau a cinq onglets : Overview (état, dossier, mémorisation de la clé, appareils avec leur préfixe d'id, dernière publication, « this device », « inactive »), Conflicts, Issues, Received hosts, Received snippets ; les nombres en couleur d'avertissement. Quand la synchronisation est désactivée, Overview l'explique (seuls des fichiers chiffrés quittent la machine ; clés privées et `known_hosts` jamais synchronisés ; Portico n'envoie rien sur le réseau) et `↵` ouvre l'assistant. Les snippets reçus non confirmés restent consultables et confirmables même désactivée. Toutes les décisions sur un hôte ou un snippet reçu portent sur l'élément affiché (son empreinte) ; elles sont armées : touches ignorées 400 ms après un changement de sélection ou de contenu, et tant qu'un rechargement (`SyncPendingChanged`) est en cours. Aucune confirmation groupée.",
    rows: &[
        (
            "Onglet suivant / précédent ; onglet n",
            "`Tab` / `Shift-Tab`, `→` / `←` ; `1`-`5`",
        ),
        (
            "Se déplacer ; faire défiler un texte long (commande, détails)",
            "`j` / `k`, `↓` / `↑` ; `PgDn` / `PgUp`, `J` / `K`",
        ),
        ("Overview : synchroniser maintenant", "`s`"),
        (
            "Overview : changer la passphrase (« Changing the passphrase does not protect against one that already leaked… », F2)",
            "`c`",
        ),
        (
            "Overview : réinitialiser la passphrase depuis cet appareil (puis confirmation armée : seul `y` réinitialise ; les autres appareils affichent une alerte à acquitter)",
            "`R`",
        ),
        ("Overview : renommer cet appareil", "`r`"),
        (
            "Overview : mémoriser ou oublier la clé sur cet appareil",
            "`m`",
        ),
        (
            "Overview : désactiver (« Forget the key on this device » coché, « Remove this device's file from the sync folder » non coché ; `Espace` coche, `↵` désactive ; les données locales restent)",
            "`D`",
        ),
        (
            "Overview : déverrouiller (état « Sync locked »)",
            "`↵` ou `u`",
        ),
        (
            "Overview : acquitter l'alerte « passphrase changed or reset on <appareil> »",
            "`a`",
        ),
        (
            "Conflicts : choisir (Union présélectionnée pour les tags, « Keep both » pour les snippets ; les champs qui diffèrent en texte principal) ; appliquer ; conflit suivant / précédent",
            "`j` / `k` ; `↵` ; `]` / `[`",
        ),
        (
            "Issues : « Skip on this device » quand c'est possible (confirmation armée, seul `y`)",
            "`x`",
        ),
        (
            "Received hosts : confirmer / refuser la destination affichée (actuelle et reçue côte à côte : HostName, User, Port, IdentityFile, ProxyJump, valeurs entières)",
            "`y` / `n`",
        ),
        (
            "Received snippets : confirmer la commande affichée en entier",
            "`y`",
        ),
        (
            "Formulaires (passphrase, nom) : champ suivant ; générer une passphrase (6 mots EFF, montrée une fois) ; la copier (OSC 52) ; une autre ; revenir à la saisie",
            "`Tab` ; `Ctrl-G` ; `c` ; `g` ; `Backspace`",
        ),
        ("Fermer (un formulaire : revenir)", "`Esc`, `q`, `Ctrl-C`"),
    ],
    outro: "Assistant « Set up sync » : explication, dossier (saisie, `Tab` complète les dossiers, `↵` demande au moteur ce qu'il contient : « No Portico sync here yet. », « A Portico sync with N devices. », incomplet, absent, refusé avec son indice), puis premier appareil (passphrase et confirmation, jauge de force du moteur, `Ctrl-G` génère, « I wrote it down » à cocher pour une passphrase générée, nom de l'appareil, « Remember the key on this device » coché) ou rejoindre (passphrase, puis revue : hôtes, détails, snippets, réglages, conflits, « nécessite une version plus récente » ; choix Review each / Prefer the folder's / Prefer this device's, qui ne confirment jamais un hôte reçu ; jeton valable 10 min, une fois). Avant l'étape qui écrit, l'avertissement de récupération exige « I understand » (`Espace`). Un refus du moteur s'affiche sous le champ concerné (code et indice). La barre de statut de la liste montre `⇅` et l'état (« Synced 2 min ago », « Syncing… », « Waiting for <appareil> », « Sync folder unavailable », « Sync locked », « Sync off » quand il y a la place), puis conflits et éléments à revoir en couleur d'avertissement, `▲` en couleur d'erreur pour une alerte. Un hôte dont la destination reçue attend porte `⇅` dans la liste ; ses détails disent « changed by <appareil> » ; un bandeau « Sync » sous les détails compte ce qui attend. Un snippet non confirmé porte « ⇅ to review » ; l'envoyer ouvre sa revue (commande entière) au lieu d'une erreur, puis revient aux snippets.",
};

/// Fixed keys of the configurable contexts (not rebindable).
const FIXED: &[(Context, &str, &str)] = &[
    (Context::Hosts, "Aller à l'onglet n", "`1`-`9`"),
    (Context::Hosts, "Vider la recherche", "`Esc`"),
    (Context::Hosts, "Quitter", "`Ctrl-C`"),
    (
        Context::Hosts,
        "Aide : page suivante, quand elle ne tient pas à l'écran (80×24 : hôtes, préfixe, journaux et édition, puis fichiers et panneaux)",
        "`Tab`, `←` / `→` (aide ouverte)",
    ),
    (
        Context::Hosts,
        "Préfixe des commandes d'onglet (comme dans un terminal)",
        "`Ctrl-Space`",
    ),
    (Context::Prefix, "Onglet n", "`1`-`9`"),
    (
        Context::Prefix,
        "Envoyer `Ctrl-Space` au programme distant",
        "`Ctrl-Space` deux fois",
    ),
    (
        Context::Prefix,
        "Annuler le préfixe",
        "`Esc` (ou toute touche non liée)",
    ),
    (
        Context::Terminal,
        "Coller",
        "collage du terminal hôte (bracketed paste si le programme distant l'active)",
    ),
];

fn context_section(out: &mut String, contexts: &[Context], title: &str, intro: &str) {
    let defaults = Keymap::defaults();
    let _ = writeln!(out, "### {title}\n");
    if !intro.is_empty() {
        let _ = writeln!(out, "{intro}\n");
    }
    out.push_str("| Action | Touches | Identifiant |\n|---|---|---|\n");
    for b in BINDINGS.iter().filter(|b| contexts.contains(&b.context)) {
        // A pane command follows the prefix.
        let lead = if b.context == Context::TerminalPrefix {
            "Ctrl-Space "
        } else {
            ""
        };
        let keys: Vec<String> = defaults
            .keys(b.id)
            .iter()
            .map(|k| format!("`{lead}{}`", k.display().replace('|', "\\|")))
            .collect();
        let _ = writeln!(out, "| {} | {} | `{}` |", b.doc, keys.join(", "), b.id);
    }
    for (_, action, keys) in FIXED.iter().filter(|(c, ..)| contexts.contains(c)) {
        let _ = writeln!(out, "| {action} | {keys} | (fixe) |");
    }
    out.push('\n');
}

fn panel_section(out: &mut String, panel: &Panel) {
    let _ = writeln!(out, "### {}\n", panel.title);
    if !panel.intro.is_empty() {
        let _ = writeln!(out, "{}\n", panel.intro);
    }
    out.push_str("| Action | Touches |\n|---|---|\n");
    for (action, keys) in panel.rows {
        let _ = writeln!(out, "| {action} | {keys} |");
    }
    out.push('\n');
    if !panel.outro.is_empty() {
        let _ = writeln!(out, "{}\n", panel.outro);
    }
}

/// The generated Markdown (without the markers).
pub(crate) fn block() -> String {
    let mut out = String::from(
        "<!-- Généré depuis apps/portico-tui/src/keymap.rs et keydoc.rs : ne pas éditer à la main.\n     cargo run -p portico-tui -- --write-keybindings docs/KEYBINDINGS.md -->\n\n",
    );
    context_section(
        &mut out,
        &[Context::Hosts],
        "TUI : liste des hôtes",
        "Sans recherche, la liste est organisée en sections : « Recent » (5 derniers hôtes encore présents dans la config), puis les groupes (ordre alphabétique), puis « Other » ; sans groupe ni récent, elle reste plate. Les touches de cette table sont configurables (voir « Configurer les raccourcis »).",
    );
    panel_section(&mut out, &SEARCH);
    context_section(
        &mut out,
        &[Context::Prefix],
        "TUI : préfixe `Ctrl-Space`",
        "Dans un terminal, toutes les touches partent vers l'hôte distant, y compris `Ctrl-C`. Les commandes Portico passent par le préfixe **`Ctrl-Space`** (la barre de statut affiche `PREFIX`). Le préfixe fonctionne aussi depuis la liste des hôtes et les panneaux, avec la même table. Il s'applique à une seule touche.",
    );
    context_section(
        &mut out,
        &[Context::Terminal, Context::TerminalPrefix],
        "TUI : terminal",
        "Les touches configurables d'un terminal doivent porter `Ctrl`, `Alt` ou `Shift` : les autres appartiennent au programme distant. Les commandes de panneau (`terminal.search`) suivent le préfixe `Ctrl-Space` et, dans un panneau terminal, priment sur la commande du préfixe de même touche (`/` cherche alors dans l'historique, plus dans la liste des hôtes).",
    );
    for panel in [
        &SCROLLBACK,
        &PANES,
        &HOST_FORM,
        &EDIT,
        &DEBUG,
        &FILES,
        &TUNNELS,
        &KEYS,
        &SNIPPETS,
        &RECORDINGS,
        &UPDATES,
        &SYNC,
    ] {
        panel_section(&mut out, panel);
    }
    // One trailing newline before the end marker.
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

/// `text` with its generated block replaced (`None`: markers missing).
pub(crate) fn replace_block(text: &str) -> Option<String> {
    let start = text.find(BEGIN)?;
    let end = text.find(END)?;
    if end < start {
        return None;
    }
    let head = text.get(..start + BEGIN.len())?;
    let tail = text.get(end..)?;
    Some(format!("{head}\n{}{tail}", block()))
}

/// Regenerates the block of the file at `path`.
pub(crate) fn write(path: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let updated = replace_block(&text)
        .ok_or_else(|| format!("{} has no `{BEGIN}` … `{END}` block", path.display()))?;
    if updated != text {
        std::fs::write(path, updated)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keybindings_doc_is_up_to_date() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/KEYBINDINGS.md");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        let expected = replace_block(&on_disk).expect("markers in docs/KEYBINDINGS.md");
        assert!(
            on_disk == expected,
            "docs/KEYBINDINGS.md is stale: run `cargo run -p portico-tui -- --write-keybindings docs/KEYBINDINGS.md`"
        );
    }

    #[test]
    fn block_lists_every_action_and_keeps_free_text() {
        let generated = block();
        for b in BINDINGS {
            assert!(generated.contains(&format!("`{}`", b.id)), "{}", b.id);
        }
        let doc = format!("intro\n{BEGIN}\nold\n{END}\noutro\n");
        let new = replace_block(&doc).unwrap();
        assert!(new.starts_with("intro\n"));
        assert!(new.ends_with(&format!("{END}\noutro\n")));
        assert!(!new.contains("\nold\n"));
        assert_eq!(replace_block(&new).as_deref(), Some(new.as_str()), "stable");
        assert_eq!(replace_block("no markers"), None);
    }
}
