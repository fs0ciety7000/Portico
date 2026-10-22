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

const EDIT: Panel = Panel {
    title: "TUI : édition d'un hôte",
    intro: "« Edit <alias> » (`e` depuis la liste). Tags, groupe et notes vivent dans `hosts.toml` ; Portico n'écrit jamais `~/.ssh/config`. Les tags se tapent librement, séparés par des espaces ou des virgules, `#` facultatif, et s'affichent en chips ; ils sont validés dès la saisie avec les règles du moteur : le tag refusé est marqué en couleur d'erreur et la raison s'affiche sous le champ, avec son hint.",
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
        ("Changer de panneau", "`Tab` / `Shift-Tab`"),
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

fn context_section(out: &mut String, context: Context, title: &str, intro: &str) {
    let defaults = Keymap::defaults();
    let _ = writeln!(out, "### {title}\n");
    if !intro.is_empty() {
        let _ = writeln!(out, "{intro}\n");
    }
    out.push_str("| Action | Touches | Identifiant |\n|---|---|---|\n");
    for b in BINDINGS.iter().filter(|b| b.context == context) {
        let keys: Vec<String> = defaults
            .keys(b.id)
            .iter()
            .map(|k| format!("`{}`", k.display()))
            .collect();
        let _ = writeln!(out, "| {} | {} | `{}` |", b.doc, keys.join(", "), b.id);
    }
    for (_, action, keys) in FIXED.iter().filter(|(c, ..)| *c == context) {
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
        Context::Hosts,
        "TUI : liste des hôtes",
        "Sans recherche, la liste est organisée en sections : « Recent » (5 derniers hôtes encore présents dans la config), puis les groupes (ordre alphabétique), puis « Other » ; sans groupe ni récent, elle reste plate. Les touches de cette table sont configurables (voir « Configurer les raccourcis »).",
    );
    panel_section(&mut out, &SEARCH);
    context_section(
        &mut out,
        Context::Prefix,
        "TUI : préfixe `Ctrl-Space`",
        "Dans un terminal, toutes les touches partent vers l'hôte distant, y compris `Ctrl-C`. Les commandes Portico passent par le préfixe **`Ctrl-Space`** (la barre de statut affiche `PREFIX`). Le préfixe fonctionne aussi depuis la liste des hôtes et les panneaux, avec la même table. Il s'applique à une seule touche.",
    );
    context_section(
        &mut out,
        Context::Terminal,
        "TUI : terminal",
        "Les touches configurables d'un terminal doivent porter `Ctrl`, `Alt` ou `Shift` : les autres appartiennent au programme distant.",
    );
    for panel in [&EDIT, &DEBUG, &FILES, &TUNNELS, &KEYS, &SNIPPETS] {
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
