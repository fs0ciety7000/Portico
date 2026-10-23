// The opt-in update check (0.3.0). The core makes the only request, and only
// once `[updates] check = true`; the view shows what it says: the one-time
// question (`check: null`), the status-bar badge (`available`), the update
// dialog and the Settings toggle. Notify only: nothing is downloaded or run.
import type { EngineEvent, UpdateInstruction, UpdateStatus } from "../api";
import { backend } from "../backend";
import { workspace } from "./workspace.svelte";

/** The question, word for word as in the TUI. */
export const UPDATE_QUESTION = "Check for new versions once a day? Portico would contact github.com and send nothing else.";

class UpdatesState {
  status: UpdateStatus | null = $state(null);
  instruction: UpdateInstruction | null = $state(null);
  /** The update dialog is open. */
  open: boolean = $state(false);
  /** An answer is being saved (the banner's buttons wait). */
  saving: boolean = $state(false);

  /** Not asked yet: the banner asks once. */
  readonly ask: boolean = $derived(this.status?.check === null);
  readonly available = $derived(this.status?.available ?? null);
  readonly on: boolean = $derived(this.status?.check === true);

  /** At startup: the core starts its checks only if they are on. */
  async init() {
    try {
      this.#set(await backend.startUpdateChecks());
    } catch {
      // settings.toml unreadable: its own notice says so; nothing is asked
      // (the answer could not be saved) nor shown.
    }
  }

  /** After lagged engine events. */
  async resync() {
    try {
      this.#set(await backend.updateStatus());
    } catch {
      // Same as `init`.
    }
  }

  onEvent(ev: EngineEvent) {
    if (ev.type === "update") this.#set(ev.status);
  }

  #set(status: UpdateStatus) {
    const before = this.status?.available?.version;
    this.status = status;
    if (!status.available) {
      this.instruction = null;
      this.open = false;
    } else if (status.available.version !== before || !this.instruction) {
      backend.updateInstruction().then(
        (i) => (this.instruction = i),
        () => (this.instruction = null),
      );
    }
  }

  /** "Check daily" / "Not now" in the banner, or the Settings toggle. */
  async setCheck(on: boolean) {
    if (this.saving) return;
    this.saving = true;
    try {
      this.#set(await backend.setUpdateCheck(on));
    } catch (e) {
      workspace.notify(e);
    } finally {
      this.saving = false;
    }
  }

  show() {
    if (this.available) this.open = true;
  }

  close() {
    this.open = false;
  }

  /** The release page, through `open_url` (validated again in Rust). */
  async openNotes() {
    const url = this.available?.url;
    if (!url) return;
    try {
      await backend.openUrl(url);
    } catch (e) {
      workspace.notify(e, "warning");
    }
  }
}

export const updates = new UpdatesState();

/** `3 h ago`, `2 days ago`… from an RFC 3339 date. */
export function ago(iso: string | null, now = Date.now()): string | null {
  if (!iso) return null;
  const at = Date.parse(iso);
  if (Number.isNaN(at)) return null;
  const minutes = Math.max(0, Math.floor((now - at) / 60_000));
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  if (minutes < 48 * 60) return `${Math.floor(minutes / 60)} h ago`;
  return `${Math.floor(minutes / 1440)} days ago`;
}
