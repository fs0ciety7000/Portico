---
name: tui-patterns
description: Patterns pour la TUI Portico (ratatui 0.30 + crossterm + alacritty_terminal). Couvre l'architecture Action/update/view, l'état, la gestion des entrées et du focus, la vue terminal embarquée, l'explorateur SFTP, les snippets, les formulaires (génération de clés, prompts) et les tests. À charger avant de travailler dans apps/portico-tui.
---

# Patterns TUI : Portico

## Architecture (déjà en place)

```
crossterm EventStream ─┐
Engine events (bcast) ─┼─▶ tokio::select! ─▶ Action ─▶ App::update(&mut self) ─▶ terminal.draw(ui::render)
mpsc<Action> interne ──┤                               (pur, sans I/O)            (pur, sans I/O)
ticker (si animation) ─┘
```

- `action.rs` est l'**unique** vocabulaire des changements d'état. Un handler clavier ne mute jamais l'état directement.
- `keymap.rs` fait `KeyEvent` + contexte (focus, overlay) → `Option<Action>`. C'est la source de vérité des raccourcis, avec `HELP`.
- `app.rs` contient `update()` (transitions) ; les effets (appels engine) partent dans `tokio::spawn` et reviennent en `Action`.
- `ui/*.rs` regroupe des fonctions `render(&App, &mut Frame, Rect)` pures. `ListState` est la seule mutation tolérée (scroll).
- On **redessine seulement après une Action**. Le ticker (80 ms) n'est actif que si `needs_animation()` est vrai (spinner, toast, curseur).

## Focus et entrées

- `Focus` est un enum explicite (`Hosts`, `Search`, puis `Terminal`, `Sftp`, `Form`…). Un seul widget reçoit le texte.
- La priorité de dispatch va de l'overlay modal au focus courant, puis aux raccourcis globaux. `Ctrl-C` quitte, sauf en focus `Terminal`, où il est transmis au remote.
- **Terminal** : toutes les touches partent au remote, sauf le préfixe `Ctrl-Space` qui ouvre le mode commande Portico
  (`Ctrl-Space n` nouvel onglet, `Ctrl-Space 1..9` onglet, `Ctrl-Space s` snippets, `Ctrl-Space f` SFTP, `Ctrl-Space d` détacher le focus).
- L'encodage des touches vers les octets suit xterm (flèches selon le mode applicatif DECCKM, `Alt` en préfixe ESC, F-keys).
  On s'appuie sur le `TermMode` d'alacritty pour le mode applicatif.
- On ignore `KeyEventKind::Release` et `Repeat` selon le contexte. On active le *bracketed paste* et on transmet `Event::Paste` en bloc.

## Vue terminal embarquée (`alacritty_terminal`)

```rust
struct TerminalView {
    term: alacritty_terminal::Term<EventProxy>,     // grille + scrollback
    parser: alacritty_terminal::vte::ansi::Processor,
    handle: TerminalHandle,                          // data plane portico-core
}
```

- La sortie suit `handle.output.recv()`, puis `parser.advance(&mut term, &bytes)`, puis `Action::TerminalDirty(id)`.
  Les octets sont **coalescés** : on draine tout le canal disponible avant de redessiner, à 60 fps au maximum.
- Le rendu passe par un widget custom qui parcourt `term.renderable_content()`, convertit chaque cellule en `Buffer` ratatui,
  et mappe les couleurs indexées 0..15 via `theme.ansi`, 16..255 via la palette xterm, et le true color directement.
  Le curseur dépend de `CursorShape`, et le texte en sélection passe en `bg_selected`.
- Un resize met à jour la taille de `Term` **et** envoie `TerminalInput::Resize` (avec debounce de 50 ms).
- Le scrollback se parcourt avec `Shift-PgUp/PgDn`, et un indicateur « ↑ 120 lignes » apparaît hors du bas.
- Les événements alacritty (`Title`, `Bell`, `ClipboardStore`) passent par `EventProxy` puis deviennent des `Action`.
  OSC 52 (presse-papiers) demande **confirmation**.

## Explorateur SFTP

- Deux panneaux, local à gauche et distant à droite, avec `Tab` pour changer de panneau. Navigation : `↵` entre, `Backspace` remonte.
- Colonnes nom, taille (`tnum`, unités IEC), date et mode. Les dossiers passent en premier (`sftp::sort_entries`).
- Actions : `F5`/`c` copie vers l'autre panneau, `F6`/`m` déplace, `F7` mkdir, `F8`/`Del` supprime (**confirmation**), `r` renomme.
- Les transferts sont listés dans une file en bas avec une barre de progression (`Gauge`, accent), le débit et l'ETA. `x` annule.
- La liste est chargée de manière asynchrone, avec un skeleton ; une erreur de permission devient un toast et un état d'erreur dans le panneau.

## Snippets

- Un picker (`Ctrl-Space s` ou palette) propose une recherche floue sur nom et commande, filtrée par `Snippet::applies_to(host)`.
- S'il y a des placeholders, un **formulaire** propose un champ par `{{name}}` (avec les dernières valeurs mémorisées), puis l'aperçu rendu.
- `↵` envoie dans l'onglet actif, avec un `\n` si `run`. On n'exécute jamais rien hors de la session.

## Formulaires (génération de clés, prompts, copy-id)

Composant `Form` réutilisable : une liste de `Field` (`Text`, `Secret`, `Select`, `Toggle`), avec `Tab`/`Shift-Tab` pour naviguer.

- **Génération de clé** :
  - type (`Select` : Ed25519 *(recommandé)*, RSA 3072/4096, ECDSA P-256/384/521) ;
  - nom de fichier (validation : n'existe pas encore) ;
  - commentaire ;
  - passphrase et confirmation (`Secret`, avec indicateur de robustesse) ;
  - « ajouter à l'agent » (`Toggle`).
- **Champs secrets** : on affiche `•` et jamais la longueur réelle au-delà de l'indicateur. Le buffer est un `Zeroizing<String>`, effacé à la fermeture.
- Validation en ligne : message sous le champ en `status-error`, et bouton primaire désactivé tant que le formulaire est invalide.
- **Prompts du moteur** (`EngineEvent::Prompt`) : modale prioritaire, quel que soit le focus.
  - `UnknownHostKey` : empreinte en mono, `y` accepter et enregistrer, `n` refuser.
  - `ChangedHostKey` : bloc rouge, **aucune** option d'acceptation, seulement `Esc`.
  - `Password` / `KeyPassphrase` : champ `Secret` et case « mémoriser dans le trousseau ».
  - En réponse, on appelle `engine.respond(id, …)` ; à la réception de `PromptResolved`, on ferme la modale (même si c'est une autre UI qui a répondu).

## Mise en page

- Le layout se construit avec `Layout::vertical/horizontal([...]).areas(rect)` (déstructuration typée), sans indexation.
- Bordures `BorderType::Rounded` et `border_subtle`, accent si focus ; padding horizontal ≥ 1.
- Tout doit rester utilisable en **80×24** : en dessous de 100 colonnes, le panneau de détails se replie sous la liste.
- Les styles viennent **uniquement** de `Styles` (`theme.rs`), jamais de `Color::` littéral dans `ui/`.

## Tests

- `ratatui::backend::TestBackend` : rendre `App` dans un état donné et comparer le `Buffer` via un snapshot texte.
- `App::update` est pur, donc on teste les séquences d'`Action` sans terminal.
- Keymap : une table de cas `(KeyEvent, Focus) → Action`.
- Pour une vérification manuelle, on lance la TUI dans `tmux` avec un `HOME` factice, puis `tmux capture-pane -p` (voir l'historique de la phase 0).

## Logs

Jamais sur stdout ou stderr (cela casserait l'affichage). Les logs vont dans un fichier, `PORTICO_LOG=portico_core=debug`, vers `$TMPDIR/portico-tui.log`.
L'utilisateur, lui, consulte le panneau debug alimenté par `LogRing`.
