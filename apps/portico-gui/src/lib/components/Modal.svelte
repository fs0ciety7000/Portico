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
    labelledBy = "modal-title",
    armed = $bindable(false),
    armedAt = $bindable(0),
    onescape,
    onkey,
    children,
    footer,
  }: {
    title: Snippet;
    tone?: "default" | "danger";
    labelledBy?: string;
    armed?: boolean;
    /** `performance.now()` when the dialog armed. */
    armedAt?: number;
    onescape: () => void;
    /** Single-key answers (`y`, `k`…), once armed; true when handled. */
    onkey?: (key: string) => boolean;
    children: Snippet;
    footer: Snippet;
  } = $props();

  const ARM_DELAY = 400;
  let dialog: HTMLDivElement;

  onMount(() => {
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

  function onkeydown(e: KeyboardEvent) {
    // Keep every key inside the dialog (no app shortcut, no terminal input).
    e.stopPropagation();
    if (e.key === "Escape") {
      e.preventDefault();
      if (armed) onescape();
      return;
    }
    if (armed && onkey && !e.metaKey && !e.ctrlKey && !e.altKey && !(e.target instanceof HTMLInputElement) && onkey(e.key)) {
      e.preventDefault();
      return;
    }
    if (e.key !== "Tab") return;
    const items = [...dialog.querySelectorAll<HTMLElement>("input, button, [tabindex='0']")].filter(
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
    role="dialog"
    aria-modal="true"
    aria-labelledby={labelledBy}
    tabindex="-1"
    bind:this={dialog}
    {onkeydown}
    transition:scale={{ start: 0.97, duration: dur("base") }}
  >
    <header id={labelledBy}>{@render title()}</header>
    <div class="body">{@render children()}</div>
    <footer>{@render footer()}</footer>
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
    gap: var(--space-2);
    padding: var(--space-3) var(--space-5);
    border-top: 1px solid var(--border-subtle);
  }
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
  .modal :global(.title-host) { margin-left: var(--space-2); color: var(--text-muted); font-weight: 450; }
</style>
