<script lang="ts">
  // Modal shell: scrim, focus trap, focus restored on close, Esc to dismiss.
  // `armed` becomes true only after ARM_DELAY *without any keystroke*: a
  // user still typing into a shell (in this tab or another) can never answer
  // a security question or type into a password field by accident.
  import { onMount, type Snippet } from "svelte";
  import { fade, scale } from "svelte/transition";
  import { dur } from "../motion";

  let {
    title,
    tone = "default",
    size = "default",
    labelledBy = "modal-title",
    armed = $bindable(false),
    armedAt = $bindable(0),
    onescape,
    onkey,
    onshortcut,
    scroll = true,
    footerWrap = true,
    children,
    footer,
  }: {
    title: Snippet;
    tone?: "default" | "danger";
    /** `wide` for list views (keys, snippets). */
    size?: "default" | "wide";
    labelledBy?: string;
    armed?: boolean;
    /** `performance.now()` when the dialog armed. */
    armedAt?: number;
    onescape: () => void;
    /** Single-key answers (`y`, `k`…), once armed; true when handled. */
    onkey?: (key: string) => boolean;
    /** Modified shortcuts (`Ctrl-N`…), once armed, from any element of the
     * dialog (fields included); true when handled. */
    onshortcut?: (e: KeyboardEvent) => boolean;
    /** `false`: the body does not scroll as a whole; the content lays out
     * its own scrolling part (a flex column: give it `min-height: 0`). */
    scroll?: boolean;
    /** `false`: the footer stays on one line (short labels, small buttons). */
    footerWrap?: boolean;
    children: Snippet;
    footer: Snippet;
  } = $props();

  const ARM_DELAY = 400;
  let dialog: HTMLDivElement;

  onMount(() => {
    // A remounted dialog (new error, new question) arms again from scratch,
    // even when the parent's bound flag is still set from the last one.
    armed = false;
    const previous = document.activeElement as HTMLElement | null;
    const focusField = () => (dialog.querySelector<HTMLElement>("[data-autofocus]") ?? dialog).focus();
    // Nothing reaches the views behind the dialog, even if the webview left
    // the focus there (WebKitGTK may ignore an early focus()).
    let timer: ReturnType<typeof setTimeout> | undefined;
    const arm = () => {
      armed = true;
      armedAt = performance.now();
      const el = document.activeElement;
      if (!dialog.contains(el) || el === dialog) focusField();
    };
    const restart = () => {
      clearTimeout(timer);
      timer = setTimeout(arm, ARM_DELAY);
    };
    const guard = (e: KeyboardEvent) => {
      // Still typing: wait for a pause before arming.
      if (!armed) restart();
      if (dialog.contains(e.target as Node)) return;
      e.preventDefault();
      e.stopPropagation();
      if (armed) {
        if (e.key === "Escape") onescape();
        // The focused control went away (a footer that changed with the
        // dialog's state): the key still means what it means in the dialog.
        else if (onkey && !e.metaKey && !e.ctrlKey && !e.altKey && onkey(e.key)) return;
        else focusField();
      }
    };
    window.addEventListener("keydown", guard, true);
    // The dialog takes the keyboard at once; its first field only once armed.
    dialog.focus();
    restart();
    return () => {
      window.removeEventListener("keydown", guard, true);
      clearTimeout(timer);
      // Give the focus back only if nobody else took it meanwhile.
      const el = document.activeElement;
      if (!el || el === document.body || dialog.contains(el)) previous?.focus?.();
    };
  });

  const isField = (t: EventTarget | null) =>
    t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement || t instanceof HTMLSelectElement;

  function onkeydown(e: KeyboardEvent) {
    // Keep every key inside the dialog (no app shortcut, no terminal input).
    e.stopPropagation();
    if (e.key === "Escape") {
      e.preventDefault();
      if (armed) onescape();
      return;
    }
    if (armed && onshortcut && (e.ctrlKey || e.metaKey) && onshortcut(e)) {
      e.preventDefault();
      return;
    }
    if (armed && onkey && !e.metaKey && !e.ctrlKey && !e.altKey && !isField(e.target) && !(e.key === "Enter" && e.target instanceof HTMLButtonElement) && onkey(e.key)) {
      e.preventDefault();
      return;
    }
    if (e.key !== "Tab") return;
    const items = [...dialog.querySelectorAll<HTMLElement>("input, textarea, select, button, [tabindex='0']")].filter(
      (el) => !el.hasAttribute("disabled"),
    );
    const first = items[0];
    const last = items[items.length - 1];
    if (!first || !last) return;
    if (e.shiftKey && document.activeElement === first) {
      last.focus();
      e.preventDefault();
    } else if (!e.shiftKey && document.activeElement === last) {
      first.focus();
      e.preventDefault();
    }
  }
</script>

<div class="scrim" transition:fade={{ duration: dur("fast") }}></div>
<div class="center">
  <div
    class="modal"
    class:danger={tone === "danger"}
    class:wide={size === "wide"}
    role="dialog"
    aria-modal="true"
    aria-labelledby={labelledBy}
    tabindex="-1"
    bind:this={dialog}
    {onkeydown}
    transition:scale={{ start: 0.97, duration: dur("base") }}
  >
    <header id={labelledBy}>{@render title()}</header>
    <div class="body" class:fixed={!scroll}>{@render children()}</div>
    <footer class:nowrap={!footerWrap}>{@render footer()}</footer>
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: color-mix(in srgb, var(--bg-base) 72%, transparent);
    z-index: 20;
  }
  .center {
    position: fixed;
    inset: 0;
    display: grid;
    place-items: center;
    padding: var(--space-6);
    z-index: 21;
  }
  .modal {
    width: min(560px, 100%);
    background: var(--bg-overlay);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-overlay);
    outline: none;
  }
  .modal.wide {
    width: min(760px, 100%);
    display: grid;
    grid-template-rows: auto minmax(0, 1fr) auto;
    max-height: min(720px, calc(100vh - 2 * var(--space-6)));
  }
  .modal.wide .body { overflow-y: auto; }
  .modal.wide .body.fixed {
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: hidden;
  }
  .modal.danger {
    border-color: color-mix(in srgb, var(--status-error) 55%, var(--border-strong));
    border-top: 3px solid var(--status-error);
  }
  header {
    padding: var(--space-5) var(--space-5) 0;
    font-size: var(--text-lg);
    font-weight: 600;
    color: var(--text-primary);
  }
  .body {
    display: grid;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-5) var(--space-5);
    color: var(--text-secondary);
  }
  footer {
    display: flex;
    justify-content: flex-end;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-5);
    border-top: 1px solid var(--border-subtle);
  }
  footer.nowrap { flex-wrap: nowrap; }
  footer.nowrap :global(button) { white-space: nowrap; }
  /* Shared by dialog contents. */
  .modal :global(button) {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    height: 32px;
    padding: 0 var(--space-4);
    border-radius: var(--radius-md);
    font: inherit;
    transition: background var(--motion-fast) var(--ease-standard),
      opacity var(--motion-fast) var(--ease-standard),
      transform var(--motion-fast) var(--ease-standard);
  }
  .modal :global(button:active:not(:disabled)) { transform: scale(0.97); }
  .modal :global(button:disabled) { opacity: 0.5; }
  .modal :global(button.primary) {
    border: 0;
    background: var(--accent);
    color: var(--text-on-accent);
    font-weight: 600;
  }
  .modal :global(button.primary:hover:not(:disabled)) { background: var(--accent-strong); }
  .modal :global(button.primary kbd) {
    background: color-mix(in srgb, var(--text-on-accent) 12%, transparent);
    border-color: transparent;
    color: inherit;
  }
  .modal :global(button.secondary) {
    border: 1px solid var(--border-strong);
    background: transparent;
    color: var(--text-primary);
  }
  .modal :global(button.secondary:hover:not(:disabled)) { background: var(--bg-elevated); }
  .modal :global(dl) {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: var(--space-2) var(--space-4);
    margin: var(--space-2) 0 0;
    padding: var(--space-3) var(--space-4);
    background: var(--bg-elevated);
    border-radius: var(--radius-md);
  }
  .modal :global(dt) { color: var(--text-muted); font-size: var(--text-sm); }
  .modal :global(dd) {
    margin: 0;
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    color: var(--text-primary);
    word-break: break-all;
    user-select: text;
  }
  /* Fingerprints are compared character by character: never wrap them. */
  .modal :global(dd.fingerprint) {
    white-space: nowrap;
    word-break: normal;
    overflow-x: auto;
    scrollbar-width: thin;
  }
  .modal :global(p) { margin: 0; }
  /* Form parts shared by the keys, vault and snippet dialogs. */
  .modal :global(.field) { display: grid; gap: var(--space-1); }
  .modal :global(.field > span) { color: var(--text-muted); font-size: var(--text-sm); }
  .modal :global(.field input:not([type="checkbox"]):not([type="radio"])),
  .modal :global(.field textarea) {
    height: 34px;
    padding: 0 var(--space-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    color: var(--text-primary);
    font: inherit;
    outline: none;
    transition: border-color var(--motion-fast) var(--ease-standard);
  }
  .modal :global(.field textarea) {
    height: auto;
    min-height: calc(5 * 1.5em + 2 * var(--space-2));
    padding: var(--space-2) var(--space-3);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    line-height: 1.5;
    resize: vertical;
  }
  .modal :global(.field input.mono:not([type="checkbox"]):not([type="radio"])) { font-family: var(--font-mono); font-size: var(--text-sm); }
  .modal :global(.field input:focus),
  .modal :global(.field textarea:focus) { border-color: var(--accent); box-shadow: 0 0 0 1px var(--accent); }
  .modal :global(.field [aria-invalid="true"]) { border-color: var(--status-error); }
  .modal :global(.field .help) { color: var(--text-muted); font-size: var(--text-xs); }
  .modal :global(.check) {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    color: var(--text-secondary);
    font-size: var(--text-sm);
  }
  .modal :global(.check input) { accent-color: var(--accent); }
  .modal :global(.inline-error) {
    display: grid;
    gap: 2px;
    color: var(--status-error);
    font-size: var(--text-sm);
  }
  .modal :global(.inline-error .hint) { color: var(--text-secondary); }
  .modal :global(.soft-warning) { color: var(--status-warning); font-size: var(--text-sm); }
  .modal :global(.note) { color: var(--text-muted); font-size: var(--text-sm); }
  /* Segmented choice (algorithm, lifetime, scope). */
  .modal :global(.segmented) {
    display: inline-flex;
    flex-wrap: wrap;
    gap: 2px;
    padding: 2px;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    justify-self: start;
  }
  .modal :global(.segmented button) {
    height: 26px;
    padding: 0 var(--space-3);
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-secondary);
    font-size: var(--text-sm);
  }
  .modal :global(.segmented button:hover) { color: var(--text-primary); }
  .modal :global(.segmented button[aria-pressed="true"]) {
    background: var(--bg-selected);
    color: var(--text-primary);
    box-shadow: inset 0 -2px 0 var(--accent);
  }
  .modal :global(.spinner) {
    display: inline-block;
    width: 12px;
    height: 12px;
    border: 1.5px solid currentColor;
    border-right-color: transparent;
    border-radius: 50%;
    animation: modal-spin var(--motion-loop) linear infinite;
  }
  @keyframes modal-spin { to { transform: rotate(360deg); } }
  .modal :global(button.small) { height: 26px; padding: 0 var(--space-3); font-size: var(--text-sm); }
  .modal :global(.footer-note) { margin-right: auto; color: var(--text-muted); font-size: var(--text-xs); }
  .modal :global(.title-host) { margin-left: var(--space-2); color: var(--text-muted); font-weight: 450; }
</style>
