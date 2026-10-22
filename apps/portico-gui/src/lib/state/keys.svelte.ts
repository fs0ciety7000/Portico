// Keys view: local key pairs, ssh-agent identities, and the copy-id flow.
//
// Keys are designated by `id` (their file name, from Rust's own listing);
// the webview never sends a path. The copy-id flow lives here, not in a
// component, so it survives the prompts (host key, password, passphrase)
// that temporarily replace the dialog.
import { asError, type AgentKey, type CopyIdOutcome, type ErrorSummary, type LocalKey } from "../api";
import { backend } from "../backend";
import { isLive, workspace } from "./workspace.svelte";

export type CopyStep = "connecting" | "installing" | "verifying";

export interface CopyRun {
  key: LocalKey;
  alias: string;
  step: CopyStep | "done" | "failed";
  /** Step that failed (for the progress list). */
  failedAt: CopyStep | null;
  outcome: CopyIdOutcome | null;
  verified: boolean;
  error: ErrorSummary | null;
  /** Session the key is installed through (prompts on it name the copy). */
  session: number | null;
}

/** `Connecting`, `Installing`, `Verifying` (progress list, prompt context). */
export const COPY_STEP_LABEL: Record<CopyStep, string> = {
  connecting: "Connecting",
  installing: "Installing",
  verifying: "Verifying",
};

export type KeysView =
  | { kind: "list" }
  | { kind: "new" }
  | { kind: "agent-add"; key: LocalKey }
  | { kind: "agent-remove"; key: AgentKey }
  | { kind: "copy-pick"; key: LocalKey }
  | { kind: "copy" };

type Load = "loading" | "ready" | "error";

class Keys {
  open = $state(false);
  view: KeysView = $state({ kind: "list" });
  local: LocalKey[] = $state([]);
  agent: AgentKey[] = $state([]);
  localState: Load = $state("loading");
  agentState: Load = $state("loading");
  localError: ErrorSummary | null = $state(null);
  agentError: ErrorSummary | null = $state(null);
  /** Row index: local keys first, then agent identities. */
  selected = $state(0);
  copy: CopyRun | null = $state(null);
  /** The public key line shown for manual selection when the clipboard
   * refused it. */
  publicLine: { id: string; text: string } | null = $state(null);
  #copyGen = 0;
  #loadGen = 0;

  rows: number = $derived(this.local.length + this.agent.length);

  inAgent(fingerprint: string): boolean {
    return this.agent.some((a) => a.fingerprint === fingerprint);
  }

  show(view: KeysView = { kind: "list" }) {
    this.view = view;
    if (!this.open) {
      this.open = true;
      void this.refresh();
    }
  }

  close() {
    if (this.view.kind === "copy") this.hideCopy();
    this.open = false;
    this.view = { kind: "list" };
    this.publicLine = null;
  }

  async refresh() {
    const gen = ++this.#loadGen;
    this.localState = this.local.length ? this.localState : "loading";
    this.agentState = this.agent.length ? this.agentState : "loading";
    await Promise.all([
      backend.listKeys().then(
        (keys) => {
          if (gen !== this.#loadGen) return;
          this.local = keys;
          this.localState = "ready";
          this.localError = null;
        },
        (e) => {
          if (gen !== this.#loadGen) return;
          this.localState = "error";
          this.localError = asError(e);
        },
      ),
      backend.listAgentKeys().then(
        (keys) => {
          if (gen !== this.#loadGen) return;
          this.agent = keys;
          this.agentState = "ready";
          this.agentError = null;
        },
        (e) => {
          if (gen !== this.#loadGen) return;
          this.agent = [];
          this.agentState = "error";
          this.agentError = asError(e);
        },
      ),
    ]);
    this.selected = Math.min(this.selected, Math.max(this.rows - 1, 0));
  }

  /** Selects the local key `id` (after generating it). */
  select(id: string) {
    const i = this.local.findIndex((k) => k.id === id);
    if (i >= 0) this.selected = i;
  }

  async copyPublic(key: LocalKey) {
    let text: string;
    try {
      text = await backend.publicKeyText(key.id);
    } catch (e) {
      workspace.notify(e);
      return;
    }
    try {
      await navigator.clipboard.writeText(text);
      this.publicLine = null;
      workspace.notify({ code: "copied", message: `Copied the public key of ${key.id}`, hint: null }, "info");
    } catch {
      // Same fallback as the TUI: the line, ready to select by hand.
      this.publicLine = { id: key.id, text };
      workspace.notify({
        code: "clipboard",
        message: `Could not copy the public key of ${key.id}`,
        hint: "Clipboard unavailable: select the key line instead.",
      });
    }
  }

  async removeFromAgent(key: AgentKey): Promise<ErrorSummary | null> {
    try {
      await backend.agentRemove(key.fingerprint);
      await this.refresh();
      return null;
    } catch (e) {
      return asError(e);
    }
  }

  // ── Copy-id ─────────────────────────────────────────────────────────

  /** Installs `key` on `alias`: the host's live session, else a new one
   * (closed afterwards); then logs in with the key alone to prove it. */
  async startCopy(key: LocalKey, alias: string) {
    const gen = ++this.#copyGen;
    const stale = () => gen !== this.#copyGen;
    this.copy = { key, alias, step: "connecting", failedAt: null, outcome: null, verified: false, error: null, session: null };
    this.view = { kind: "copy" };
    const run = this.copy;
    let session: number | null = null;
    let opened = false;
    try {
      const live = workspace.byAlias[alias];
      if (live && isLive(live)) {
        session = live.id;
      } else {
        session = await workspace.connectQuiet(alias);
        opened = session !== null;
        if (session === null) throw { code: "connect", message: `could not connect to ${alias}`, hint: null };
      }
      run.session = session;
      const failed = await workspace.settled(session);
      if (stale()) return;
      if (failed) throw failed;
      run.step = "installing";
      run.outcome = await backend.copyId(session, key.id);
      if (stale()) return;
      if (key.hasPrivate) {
        run.step = "verifying";
        await backend.verifyKeyLogin(session, key.id);
        if (stale()) return;
        run.verified = true;
      }
      run.step = "done";
    } catch (e) {
      if (stale()) return;
      run.failedAt = run.step === "done" || run.step === "failed" ? null : run.step;
      run.error = asError(e);
      run.step = "failed";
    } finally {
      if (opened && session !== null) this.#release(session);
      if (!stale()) this.#reportIfHidden(run);
    }
  }

  /** The copy dialog is not on screen: say how the copy ended. */
  #reportIfHidden(run: CopyRun) {
    if (this.open && this.view.kind === "copy") return;
    if (run.step === "done") {
      const what = run.outcome === "already_present" ? "was already on" : "is installed on";
      workspace.notify(
        {
          code: "copy_id",
          message: `${run.key.id} ${what} ${run.alias}`,
          hint: run.verified ? "Logging in with it works." : null,
        },
        "info",
      );
    } else if (run.step === "failed" && run.error) {
      workspace.notify(run.error);
    }
  }

  retryCopy() {
    const run = this.copy;
    if (run) void this.startCopy(run.key, run.alias);
  }

  /** Leaves the progress dialog: the copy goes on (it cannot be
   * interrupted halfway safely), and a toast reports how it ended. */
  hideCopy() {
    const run = this.copy;
    if (run && (run.step === "done" || run.step === "failed")) {
      this.dismissCopy();
      return;
    }
    this.view = { kind: "list" };
  }

  /** Forgets a finished copy (Done / Close). */
  dismissCopy() {
    this.#copyGen++;
    this.copy = null;
    this.view = { kind: "list" };
  }

  /** The prompt belongs to the running copy (its session, or the
   * verification login on the same host). */
  copyContext(session: number | null, alias: string | undefined): string | null {
    const run = this.copy;
    if (!run || run.step === "done" || run.step === "failed") return null;
    const ours = (session !== null && session === run.session) || (run.step !== "installing" && alias === run.alias);
    return ours ? `Copy ${run.key.id} to ${run.alias} · ${COPY_STEP_LABEL[run.step]}` : null;
  }

  /** Closes a session opened for copy-id, unless a tab now uses it. */
  #release(session: number) {
    if (workspace.tabs.some((t) => t.session === session)) return;
    if (isLive(workspace.sessions[session])) backend.disconnect(session).catch(() => {});
  }
}

export const keys = new Keys();

/** `in 3 days`, `in 5 h`, `in 12 min` (rough, for badges). */
export function relative(secs: number): string {
  const s = Math.max(0, Math.round(secs));
  if (s < 3600) return `${Math.max(1, Math.round(s / 60))} min`;
  if (s < 86_400 * 2) return `${Math.round(s / 3600)} h`;
  return `${Math.round(s / 86_400)} days`;
}

/** `21 Oct 2026` for a Unix time. */
export function shortDate(unix: number): string {
  return new Date(unix * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
}

/** `ssh-ed25519` → `ED25519`, `ecdsa-sha2-nistp256` → `ECDSA 256`. */
export function algorithmLabel(algorithm: string): string {
  if (algorithm.includes("ed25519")) return "ED25519";
  const ec = /nistp(\d+)/.exec(algorithm);
  if (ec) return `ECDSA ${ec[1]}`;
  if (algorithm.startsWith("rsa") || algorithm.includes("rsa")) return "RSA";
  return algorithm.toUpperCase();
}
