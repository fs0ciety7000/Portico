// Snippets: list, editor, and runs into the active terminal.
//
// Scope rules, placeholder parsing, rendering and the bytes typed into the
// terminal all live in Rust (`portico-core`); the webview sends a snippet id
// and the placeholder values only.
import { asError, type ErrorSummary, type Snippet, type SnippetDraft } from "../api";
import { backend } from "../backend";
import { sync, UNCONFIRMED } from "./sync.svelte";
import { workspace } from "./workspace.svelte";

/** Where a snippet would run: the active tab's terminal, if it has a shell. */
export interface SnippetTarget {
  alias: string;
  /** Null while the tab has no open shell. */
  terminal: number | null;
  /** The terminal's bracketed paste mode (DECSET 2004), read at run time. */
  bracketed: () => boolean;
}

export type SnippetsView =
  | { kind: "list" }
  | { kind: "edit"; draft: SnippetDraft }
  | { kind: "delete"; snippet: Snippet }
  | { kind: "run"; snippet: Snippet; error: ErrorSummary | null };

type Load = "loading" | "ready" | "error";

/** The core refused to paste a multi-line command without bracketed paste
 * (its lines would run one by one): offer to run it instead. */
export const NEEDS_PASTE_MODE = "snippet_needs_paste_mode";

export const emptyDraft = (alias: string | null): SnippetDraft => ({
  id: "",
  name: "",
  command: "",
  description: null,
  scope: alias ? { kind: "hosts", values: [alias] } : { kind: "global" },
  run: true,
});

class Snippets {
  open = $state(false);
  view: SnippetsView = $state({ kind: "list" });
  all: Snippet[] = $state([]);
  /** Ids applicable to `alias` (from the core's scope rules). */
  applicable: string[] = $state([]);
  /** Host the list is shown for (active tab, else selected host). */
  alias: string | null = $state(null);
  state: Load = $state("loading");
  /** List filter and selection (kept while switching views). */
  filter = $state("");
  selected = $state(0);
  error: ErrorSummary | null = $state(null);
  /** Applicable snippets for the palette ("Run: …"), by alias. */
  palette: { alias: string; list: Snippet[] } | null = $state(null);
  #gen = 0;

  show(alias: string | null, view: SnippetsView = { kind: "list" }) {
    this.view = view;
    if (alias !== this.alias) {
      this.filter = "";
      this.selected = 0;
    }
    this.alias = alias;
    this.open = true;
    void this.load();
  }

  close() {
    this.open = false;
    this.view = { kind: "list" };
  }

  async load() {
    const gen = ++this.#gen;
    const alias = this.alias;
    if (!this.all.length) this.state = "loading";
    try {
      const [all, applicable] = await Promise.all([
        backend.listSnippets(),
        alias ? backend.snippetsFor(alias) : backend.listSnippets(),
      ]);
      if (gen !== this.#gen) return;
      this.all = all;
      this.applicable = applicable.map((s) => s.id);
      this.state = "ready";
      this.error = null;
      if (alias) this.palette = { alias, list: applicable };
    } catch (e) {
      if (gen !== this.#gen) return;
      this.state = "error";
      this.error = asError(e);
    }
  }

  /** Keeps the palette's "Run: …" entries in step with the active host. */
  async loadPalette(alias: string | null) {
    if (!alias) {
      this.palette = null;
      return;
    }
    try {
      const list = await backend.snippetsFor(alias);
      this.palette = { alias, list };
    } catch {
      this.palette = null;
    }
  }

  async save(draft: SnippetDraft): Promise<ErrorSummary | null> {
    try {
      const saved = await backend.saveSnippet(draft);
      await this.load();
      this.view = { kind: "list" };
      workspace.notify({ code: "saved", message: `Saved “${saved.name}”`, hint: null }, "info");
      return null;
    } catch (e) {
      return asError(e);
    }
  }

  async remove(snippet: Snippet): Promise<ErrorSummary | null> {
    try {
      await backend.deleteSnippet(snippet.id);
      await this.load();
      this.view = { kind: "list" };
      return null;
    } catch (e) {
      return asError(e);
    }
  }

  /** Types the rendered snippet into `target`. Null on success. */
  async run(
    target: SnippetTarget | null,
    snippet: Snippet,
    values: Record<string, string>,
    run: boolean | null = null,
  ): Promise<ErrorSummary | null> {
    if (!target || target.terminal === null) {
      return { code: "no_terminal", message: "no open terminal", hint: "Open a terminal tab, then run the snippet." };
    }
    try {
      await backend.runSnippet(target.terminal, snippet.id, values, target.bracketed(), run);
      return null;
    } catch (e) {
      const error = asError(e);
      // Changed by sync and not confirmed on this device (F8): its review,
      // not a bare error.
      if (error.code === UNCONFIRMED) {
        this.close();
        if (await sync.reviewSnippetById(snippet.id)) return null;
      }
      return error;
    }
  }
}

export const snippets = new Snippets();

/** `Global`, `web-01, staging`, `#prod`. */
export function scopeLabel(snippet: Pick<Snippet, "scope">): string {
  const s = snippet.scope;
  if (s.kind === "global") return "Global";
  if (s.kind === "hosts") return s.values.join(", ");
  return s.values.map((t) => `#${t}`).join(" ");
}
