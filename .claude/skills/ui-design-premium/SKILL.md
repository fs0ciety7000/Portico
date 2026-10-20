---
name: ui-design-premium
description: Design system de Portico (identité architecturale minimaliste), commun à la TUI et à la GUI. Couvre couleurs et tokens, espacements, typographie, micro-interactions, composants, navigation clavier et feedback de statut de connexion. À charger avant de créer ou modifier un écran, un composant, un thème ou un token.
---

# Design premium : Portico

## Identité

Un **portique** : entablement, colonnes, arche. Le design en garde l'idée de **structure, d'ordre et de seuil**.
Les mots-clés sont *architectural, minéral, calme, précis*. Pas de dégradés criards, pas de glassmorphism gratuit, pas d'emoji dans l'UI.

- La marque se compose du logo `design/portico-mark.svg` (trait seul, accent sur fond sombre) et du wordmark `PORTICO`
  en serif espacé (`--font-display`, `letter-spacing` d'au moins 0.3em).
- **Un seul accent par thème**, réservé au focus, à la sélection, à l'action primaire et à la marque.
- **La couleur saturée = le sens** : les statuts sont la seule couleur vive de l'écran.

## Tokens (source unique : `crates/portico-theme`)

Ne **jamais** coder une couleur, une taille ou une durée en dur. En GUI on utilise `var(--…)`, en TUI `Styles` (`apps/portico-tui/src/theme.rs`).
Après une modification de `portico-theme`, lancer `cargo run -p portico-theme --bin export-tokens`, faute de quoi le test échoue.

| Rôle | Token | Usage |
|---|---|---|
| Fond app / terminal | `bg-base` | Derrière tout |
| Panneaux | `bg-surface` | Sidebar, panneaux |
| Élevé | `bg-elevated` | Inputs, survol |
| Overlay | `bg-overlay` | Modales, palette, toasts |
| Sélection | `bg-selected` | Ligne sélectionnée (avec une barre d'accent de 2 px à gauche) |
| Bordures | `border-subtle` / `border-strong` | 1 px, jamais plus |
| Texte | `text-primary` / `text-secondary` / `text-muted` | Hiérarchie à trois niveaux, pas plus |
| Accent | `accent`, `accent-strong` (hover), `text-on-accent` | |
| Statuts | `status-{idle,connecting,authenticating,connected,warning,error}` | Exclusivement pour l'état |

**Contraste** (testé dans `portico-theme`) : texte principal ≥ 7:1 (AAA), secondaire ≥ 4.5:1, statuts ≥ 4.5:1 sur `bg-surface`.
Un nouveau thème doit passer ces tests.

**Thèmes** : Obsidian (défaut, laiton), Basalt (acier), Travertine Night (terracotta). Chacun fournit aussi sa palette ANSI 16 couleurs, harmonisée avec les statuts.

## Espacement, formes, typographie

- Les espacements suivent une échelle de 4 px : `--space-1` (4), puis 8, 12, 16, 24, 32 et `--space-7` (48).
  Dans les panneaux, le padding horizontal vaut au moins `--space-3`.
- Rayons : `sm` 4 (kbd, tags), `md` 8 (inputs, lignes, boutons), `lg` 12 (modales), `pill` (badges).
- Polices : UI en **Inter** à 13 px de base ; terminal et données techniques (hôtes, chemins, ports, empreintes) en **JetBrains Mono** ;
  titres d'hôte et wordmark en **Cormorant Garamond**, avec parcimonie.
- On utilise les **chiffres tabulaires** (`tnum`) pour latence, tailles et progression.
- La hiérarchie passe d'abord par la **taille et l'espace**, ensuite par la graisse (450/550/600), et en dernier par la couleur.
- Les hauteurs de ligne sont régulières : 34 px pour les listes, 32 px pour les boutons, 24 px pour les badges.

## Mouvement

| Durée | Token | Usage |
|---|---|---|
| 120 ms | `--motion-fast` | Hover, focus, pression |
| 180 ms | `--motion-base` | Changement de contenu (fly de 6 px), badges |
| 260 ms | `--motion-slow` | Modales, panneaux |

- La courbe est `--ease-standard`. Aucune animation ne dépasse 260 ms, sauf les indicateurs en boucle (pulse, skeleton).
- On anime **opacité et transform** uniquement : jamais `width`, `height` ou `top` (layout).
- `prefers-reduced-motion` est respecté globalement (déjà dans `base.css`).
- En TUI, le mouvement passe par un spinner braille à 80 ms et un curseur clignotant. **Aucun redraw sans changement d'état.**

## Feedback de statut de connexion (composant critique)

| Phase | Couleur | Visuel |
|---|---|---|
| idle / disconnected | `status-idle` | point plein |
| resolving / connecting (hop n/m) | `status-connecting` | point **pulsé** + libellé « Connecting · bastion (1/2) » |
| verifying host key / authenticating | `status-authenticating` | point pulsé + méthode (« Authenticating · agent ») |
| connected | `status-connected` | point plein + **latence** `23 ms` (mono, muted) |
| latence > 250 ms | `status-warning` | point plein, latence colorée |
| failed | `status-error` | point plein + message court + **hint** actionnable |

- Le statut est **toujours visible** : badge dans l'en-tête de l'onglet, point dans la liste d'hôtes, barre de statut.
- L'erreur se compose d'un titre (ce qui a échoué), d'un hint (quoi faire) et d'une action (`Réessayer`, `Voir les logs`).
  On n'affiche jamais une stack trace ou une erreur brute.
- **Clé hôte modifiée** : modale rouge **bloquante**, avec les deux empreintes et la ligne de `known_hosts`, sans bouton « accepter ».

## Composants (contrats communs TUI et GUI)

- **Liste d'hôtes** : point de statut, alias en graisse 550, `★` d'accent si favori, `user@host` en mono muted aligné à droite, `#tags` secondaires.
  La sélection se voit au fond `bg-selected` et à la barre d'accent de 2 px.
- **Recherche** : toujours accessible (`/`, `⌘K`). Elle filtre en direct, et `#tag` comme `*` sont documentés dans le placeholder ou l'aide.
- **Palette de commandes** (`⌘K` / `Ctrl-K`) : chaque action de l'app y figure, avec son raccourci affiché.
- **Modales** : titre, contenu, actions à droite (primaire en accent). `Esc` ferme. Le focus est piégé et rendu à l'élément d'origine.
- **Toasts** : en bas à droite, 4 s, avec une icône et une bordure de couleur de statut. Ils ne contiennent jamais d'action destructive.
- **États obligatoires** pour toute vue de données : *chargement* (skeleton ou spinner), *vide* (phrase et action suivante), *erreur* (message et hint), *nominal*.
- **Raccourcis visibles** : `kbd` dans les boutons, barre de statut contextuelle et aide `?`.

## Clavier

- Chaque action a un raccourci, et la souris n'est jamais requise.
- Conventions : `j/k` et les flèches pour se déplacer, `/` pour chercher, `↵` pour l'action primaire, `Esc` pour reculer ou fermer, `?` pour l'aide,
  `⌘K`/`Ctrl-K` pour la palette, `t` pour le thème, et `Ctrl-Space` comme préfixe dans un terminal (le reste va au remote).
- Le focus est **toujours visible** (anneau d'accent de 2 px en GUI, bordure d'accent en TUI).
- Tout nouveau raccourci va dans la keymap (TUI `keymap.rs`, GUI palette) **et** dans `docs/KEYBINDINGS.md`.

## Checklist avant de livrer un écran

- [ ] Aucune valeur en dur, uniquement des tokens
- [ ] Les quatre états (chargement, vide, erreur, nominal) sont soignés
- [ ] Tout est utilisable au clavier, avec un focus visible et des raccourcis documentés
- [ ] Les trois thèmes ont été vérifiés (capture), et la TUI sur un terminal 80×24
- [ ] Animations ≤ 260 ms, transform/opacity, reduced-motion respecté
- [ ] Revue par `agent-design-review` avec captures
