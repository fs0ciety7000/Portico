---
name: gui-tauri-patterns
description: Patterns pour la GUI Portico (Tauri 2 + Svelte 5 runes + xterm.js). Couvre le contrat IPC avec portico-core, le streaming PTY par Channel, l'état réactif, le composant terminal, la sécurité Tauri (CSP, capabilities) et le mode démo navigateur. À charger avant de travailler dans apps/portico-gui.
---

# Patterns GUI : Portico

## Couches

```
Svelte (src/)  ──invoke()──▶  src-tauri/commands.rs ──▶ portico_core::Engine
      ▲                                │
      └── listen("engine://event") ◀───┘ (forwarder : broadcast → emit)
      └── Channel<Uint8Array>  ◀────────── TerminalHandle.output (par onglet)
```

- `src-tauri` reste **mince** : il se limite à traduire. Aucune logique métier (parsing, recherche, auth) côté Rust GUI ni côté TS.
- Les DTO sont dans `src-tauri/src/dto.rs` (`#[serde(rename_all = "camelCase")]`) et miroirs dans `src/lib/api.ts`. On modifie les deux ensemble.
- Une erreur traverse l'IPC sous forme d'`ErrorSummary { code, message, hint }`, et `asError()` la normalise côté TS.

## Streaming terminal (phase 1c)

```rust
#[tauri::command]
async fn open_terminal(engine: State<'_, Engine>, session: u64, cols: u16, rows: u16,
                       on_data: tauri::ipc::Channel<Vec<u8>>) -> Result<u64, ErrorSummary> {
    let mut h = engine.open_terminal(/* … */).await.map_err(|e| e.summary())?;
    tauri::async_runtime::spawn(async move {
        while let Some(bytes) = h.output.recv().await {
            if on_data.send(bytes.to_vec()).is_err() { break; }   // la vue a été fermée
        }
    });
    Ok(h.id.get())
}
```

- L'entrée passe par `invoke("terminal_input", { id, data })`, avec un batching par `requestAnimationFrame` si la frappe est rapide. Le resize par `invoke("terminal_resize")` avec debounce de 50 ms.
- **Jamais** d'octets PTY via `emit` (le bus global n'est ni ordonné par onglet ni binaire).

## Composant Terminal (xterm.js 6)

- `new Terminal({ fontFamily: var(--font-mono), theme: <depuis les tokens ansi-0..15>, cursorBlink: true, allowProposedApi: false })`.
- Addons : `FitAddon` (ResizeObserver) et `WebglAddon` (sur `onContextLoss`, dispose et retour au renderer DOM).
- Le thème se met à jour **à chaud** au changement de `data-theme`, en lisant les `getComputedStyle` des `--ansi-*`.
- Le focus est capturé dans le terminal. `⌘K` et `⌘T`, `⌘W`, `⌘1..9` sont interceptés par l'app (`attachCustomKeyEventHandler`), tout le reste va au remote.
- Démontage : `term.dispose()`, fermeture de l'onglet côté engine, et `unlisten`.

## État Svelte 5

- Runes uniquement : `$state`, `$derived`, `$effect`, `$props`, `$bindable`. Pas de stores legacy pour du nouvel état.
- L'état dérivé du moteur (sessions, phases, latence) vit dans un module `src/lib/state/sessions.svelte.ts`, alimenté par `onEngineEvent`.
  Les composants le lisent, et seules les actions utilisateur appellent `api.*`.
- Un `$effect` qui fait de l'async doit gérer l'obsolescence (cf. `query` dans `App.svelte`) et le nettoyage (valeur de retour).
- Le rendu des listes est toujours clé (`{#each items as x (x.id)}`).

## Design

- Charger le skill `ui-design-premium`. Uniquement des `var(--…)` (tokens générés) : aucune couleur, taille ou durée en dur.
- Les transitions passent par `svelte/transition` (`fly` y:6 à 180 ms pour un changement de contenu, `scale` 0.97 à 160 ms pour une modale, `fade` à 120 ms pour un scrim).
- Les icônes sont des SVG inline en trait de 1.5 px (`stroke: currentColor`), sans pack d'icônes lourd.
- Le titlebar est en overlay (`titleBarStyle: Overlay`, avec 84 px réservés pour les feux macOS) et `data-tauri-drag-region`.

## Sécurité Tauri

- CSP stricte dans `tauri.conf.json` : pas de `unsafe-eval`, pas de remote. Toute exception s'accompagne d'une ADR et d'une revue `agent-security`.
- `capabilities/default.json` est minimal (`core:default`). On n'ajoute aucun plugin fs/shell/http : tout passe par nos commandes typées.
- Les commandes valident leurs entrées (IDs connus, chemins SFTP normalisés côté core).
- Aucun secret ne doit traîner dans le state JS au-delà du temps de la modale : on envoie `respond` puis on remet le champ à zéro.

## Mode démo navigateur

`pnpm dev` sans Tauri sert `src/lib/demo.ts` (`isTauri()` faux), ce qui permet d'itérer sur le design et de faire des captures Playwright.
Les données de démo ne doivent jamais être utilisées dans l'app de bureau.

## Vérifications

```bash
cd apps/portico-gui && pnpm check && pnpm build
cargo clippy -p portico-gui           # Linux : libwebkit2gtk-4.1-dev
pnpm tauri dev                        # app complète
```
Les captures pour la revue design se font avec `vite preview` + Playwright (Chromium est préinstallé dans l'environnement cloud).
