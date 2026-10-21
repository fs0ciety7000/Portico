// Fixed (non-configurable) GUI keys, per view, as documented in
// docs/KEYBINDINGS.md. The configurable ones come from `shortcuts.ts`;
// `scripts/gen-keybindings.ts` renders both. Pure data (no imports): it
// runs under Node too. Wording in French, like the rest of the doc.

export interface DocTable {
  title: string;
  intro?: string;
  rows: [string, string][];
  outro?: string;
}

export const GUI_FIXED: DocTable[] = [
  {
    title: "Terminal (GUI) : touches fixes",
    intro:
      "Toutes les touches partent vers l'hôte distant, sauf les raccourcis de l'app (tableau « Application et onglets »).",
    rows: [
      ["Reconnecter un onglet terminé ou en échec", "`↵`"],
      ["Annuler une connexion en cours / revenir aux hôtes (onglet sans shell)", "`Esc`"],
      ["Barre d'onglets (focus clavier) : onglet voisin, premier / dernier, fermer", "`←` `→`, `Home` / `End`, `Suppr`"],
      ["Fermer un onglet (souris)", "clic milieu sur l'onglet"],
      ["Coller (Linux)", "`Ctrl-Shift-V`, `Shift-Insert`, clic milieu"],
    ],
    outro: "Tout collage est assaini (ESC et contrôles C1 retirés) avant d'atteindre le terminal.",
  },
  {
    title: "Liste des hôtes (GUI) : sections",
    intro:
      "Sans recherche, la liste montre « Recent » (les 5 derniers hôtes encore présents dans la config), puis un titre par groupe " +
      "(ordre alphabétique), puis « Other » ; sans groupe ni récent, une liste plate. Avec une recherche, les résultats classés, à plat. " +
      "Les titres de section sont des lignes : on s'y déplace comme sur un hôte.",
    rows: [
      ["Plier / déplier la section (sur son titre)", "`↵`, clic"],
      ["Retirer de « Recent » (souris)", "`×` au survol de la ligne"],
      ["Naviguer pendant la recherche", "`↑` `↓`, `Ctrl-N` / `Ctrl-P`"],
      ["Vider la recherche", "`Esc`"],
      ["Favori (souris)", "étoile à côté du nom, dans les détails"],
    ],
  },
  {
    title: "Édition d'un hôte (GUI)",
    intro:
      "« Edit <alias> » : tags, groupe et notes, enregistrés dans `hosts.toml` par le moteur (Portico n'écrit jamais `~/.ssh/config`). " +
      "Les erreurs s'affichent sous le champ concerné, avec un conseil.",
    rows: [
      ["Ajouter un tag (`#` facultatif)", "`Espace`, `,` ou `↵` dans le champ Tags"],
      ["Retirer le dernier tag", "`Backspace` dans le champ vide"],
      ["Groupe : choisir une suggestion (groupes existants)", "`↓` / `↑`, puis `↵`"],
      ["Champ suivant", "`Tab`"],
      ["Enregistrer", "`Ctrl-S` (macOS : `⌘S`), `Ctrl-↵`, ou `↵` dans un champ d'une ligne"],
      ["Annuler", "`Esc`"],
    ],
  },
  {
    title: "Journaux de session (GUI)",
    intro:
      "Panneau debug : sessions à gauche (vivantes et 20 dernières terminées, sondes « probe » de copy-id comprises), à droite " +
      "les journaux ou la config effective (`ssh -G`). Ouvert depuis un onglet, il présélectionne sa session. Les textes sont " +
      "affichés échappés (contrôles, bidi), comme dans « Copy all ». Les mots-clés qui exécutent des commandes (`ProxyCommand`, " +
      "`LocalCommand`, `RemoteCommand`, `KnownHostsCommand`) sont masqués et jamais copiés.",
    rows: [
      ["Session précédente / suivante", "`k` / `j`, `↑` / `↓`"],
      ["Onglet Logs / Effective config", "`1` / `2`"],
      ["Filtrer (texte ; `Esc` vide le filtre)", "`/`"],
      ["Niveau minimal", "boutons Debug, Info, Warn, Error"],
      ["Suivre en direct on/off (défiler vers le haut l'arrête)", "`f`"],
      ["Copier tout le journal (texte `to_line` ; sans presse-papiers, le texte s'affiche sélectionné)", "`c`"],
      ["Rafraîchir (et recalculer la config effective)", "`r`"],
      ["Fermer", "`Esc`"],
    ],
  },
  {
    title: "Fichiers (GUI)",
    intro:
      "Même grammaire que la TUI (`r` renommer, `d` supprimer, `x` annuler, `Ctrl-R` rafraîchir, `k`/`o`/`s` en cas de conflit) ; " +
      "chaque action figure aussi dans la palette avec sa touche. Panneau à droite du terminal de l'onglet actif. Il prend le clavier " +
      "à l'ouverture ; un clic dans le terminal le lui rend. Les fichiers à envoyer viennent du sélecteur natif ou d'un glisser-déposer " +
      "sur la fenêtre ; les téléchargements vont dans le dossier affiché en bas du panneau (« Change » ou la palette pour en choisir un autre).",
    rows: [
      ["Se déplacer", "`j` / `k`, `↓` / `↑`, `Home` / `End`"],
      ["Ouvrir le dossier, ou télécharger le fichier", "`↵`, `→`, double-clic"],
      ["Dossier parent", "`Backspace`, `←`, fil d'Ariane"],
      ["Envoyer des fichiers (sélecteur natif)", "`u`, ou déposer des fichiers sur le panneau"],
      ["Nouveau dossier (saisie en place, `↵` valide, `Esc` annule)", "`n`"],
      ["Renommer (saisie en place)", "`r` ou `F2`"],
      ["Supprimer (confirmation, focus sur `Keep` : `y` supprime, `n` / `Esc` garde)", "`d`, `F8` ou `Suppr`"],
      ["Annuler le dernier transfert en cours", "`x`"],
      ["Afficher / masquer les fichiers cachés", "`.`"],
      ["Rafraîchir (dans le panneau ; ailleurs `Ctrl-R` recharge `ssh_config`)", "`Ctrl-R`"],
      ["Destination existante", "`k` garder les deux (copie numérotée) · `o` écraser · `s` ignorer · `Esc` annuler"],
      ["Session pas encore prête / terminée", "le panneau affiche la phase, ou l'erreur avec son hint ; `↵` reconnecte"],
      ["Revenir au terminal (le panneau reste ouvert)", "clic dans le terminal ; le raccourci Fichiers ou `Esc` le ferme"],
    ],
  },
  {
    title: "Tunnels (GUI)",
    rows: [
      ["Ajouter", "saisir `-L 8080:db:5432` ou `-R 9000:localhost:3000`, puis `↵`"],
      ["Choisir un tunnel ou un forward proposé", "`↑` / `↓`"],
      ["Démarrer le forward proposé choisi / arrêter le tunnel choisi", "`↵` / `Suppr` (ou boutons `Start` / `Stop`)"],
      ["Écoute exposée (hors loopback)", "bloc d'avertissement, focus sur `Cancel` ; `Expose tunnel` actif après 400 ms sans frappe ni clic · `Esc` annule"],
      ["Fermer", "`Esc`"],
    ],
  },
  {
    title: "Clés (GUI)",
    intro:
      "Dialogue large : clés locales de `~/.ssh`, puis identités de l'agent (seules les listes défilent), puis le bandeau du vault, " +
      "toujours visible (masqué quand le trousseau système est utilisé). Le premier focus est sur la liste ; chaque action a aussi son " +
      "bouton (« Copy key », « Copy to host… », « Add to agent » / « Remove from agent », « New key ») et son entrée de palette, qui " +
      "affiche la touche (« New key » : raccourci Clés, puis `n` ; vault : `v` / `V`).",
    rows: [
      ["Se déplacer", "`j` / `k`, `↓` / `↑`, `Home` / `End`"],
      [
        "Nouvelle clé (Ed25519 par défaut, RSA 3072/4096, ECDSA P-256/384/521 ; `↵` crée ; passphrase vide ou d’au moins 8 caractères, compteur plafonné « n / 8 min » ; après un refus, la passphrase effacée doit être ressaisie, ou son absence cochée, avant de recréer)",
        "`n`",
      ],
      ["Ajouter à l'agent (durée : « Until removed », 1 h, 8 h ; passphrase demandée si la clé est chiffrée)", "`a`"],
      ["Retirer de l'agent (confirmation, focus sur `Keep` : `y` retire, `n` / `Esc` garde)", "`x`"],
      ["Copier sur un hôte (recherche d'hôte, `↑` `↓`, `↵`)", "`c`"],
      ["Copier la clé publique (presse-papiers ; s'il est indisponible, la ligne s'affiche, prête à sélectionner)", "`y`"],
      ["Vault : déverrouiller / verrouiller (ou créer s'il n'existe pas)", "`v`"],
      ["Vault : changer la passphrase (ou créer)", "`V`"],
      ["Rafraîchir (rappelé dans le titre du dialogue)", "`Ctrl-R`"],
      ["Fermer (depuis un sous-dialogue : revenir à la liste)", "`Esc`"],
    ],
    outro:
      "**Copy-id** (« Copy to host ») : étapes « Connecting · Installing · Verifying ». Réutilise la session vivante de l'hôte, sinon en " +
      "ouvre une (prompts habituels) et la ferme ensuite. Un prompt lié à la copie (clé hôte, mot de passe, passphrase de la vérification) " +
      "porte le sous-titre « Copy <clé> to <hôte> · <étape> ». Pendant la copie, `Esc` (« Hide ») masque le dialogue sans l'interrompre : " +
      "une notification dit comment elle s'est terminée. Résultat : `↵` termine, `c` copie les lignes `Host` / `IdentityFile` à ajouter " +
      "soi-même (Portico n'écrit jamais `~/.ssh/config`). Échec : message et hint, `r` ou `↵` réessaie, `Esc` ferme.\n\n" +
      "**Vault au démarrage** : verrouillé, Portico demande la passphrase maître une fois ; `Esc` (« Continue without ») laisse les " +
      "secrets non mémorisés jusqu'au déverrouillage (`v` dans Clés). Cocher « Remember » sans vault (ou vault verrouillé) propose " +
      "ensuite de le créer (ou de le déverrouiller).",
  },
  {
    title: "Snippets (GUI)",
    rows: [
      ["Exécuter depuis la palette (une entrée « Run: <nom> » par snippet de l'hôte de l'onglet actif)", "palette"],
      ["Se déplacer", "`j` / `k`, `↓` / `↑`, `Home` / `End`"],
      ["Filtrer (dans le filtre : `↑` `↓`, `↵` exécute, `Esc` vide puis rend la liste)", "`/`"],
      ["Exécuter (« Run ») ou coller (« Paste ») dans l'onglet actif (formulaire des placeholders si besoin, `↵` envoie)", "`↵`, double-clic"],
      ["Nouveau / éditer / supprimer (aussi depuis le filtre)", "`n` / `e` / `d`, ou `Ctrl-N` / `Ctrl-E` / `Ctrl-D`"],
      ["Enregistrer dans l'éditeur (`↵` dans un champ d'une ligne aussi)", "`Ctrl-S` (macOS : `⌘S`), `Ctrl-↵` (`⌘↵`)"],
      ["Confirmer la suppression (focus sur `Keep` : `y` supprime, `n` / `Esc` garde)", "`y`"],
      [
        "Snippet multi-ligne « paste only » refusé sans bracketed paste (garde-fou, encadré d'avertissement)",
        "`r` « Run it » (exécute chaque ligne, Entrée envoyée), après réarmement (400 ms sans frappe) ; `↵` ne fait rien ; `Esc` annule",
      ],
      ["Fermer", "`Esc`"],
    ],
    outro:
      "Sans terminal ouvert, la liste reste éditable et l'exécution est désactivée ; la bannière « No open terminal » donne l'action " +
      "suivante (« `Esc`, then `↵` to connect <hôte> », ou « reconnect » pour un onglet terminé). Les snippets hors portée de l'hôte " +
      "sont affichés en retrait (« not for <hôte> ») et ne s'exécutent pas.",
  },
];
