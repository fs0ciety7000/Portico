// "Import hosts…" (Termius or Termix, detected from the content): native
// picker (Rust), preview of the plan, then the import and its report. The
// plan, key material included, stays in Rust under a token; this state only
// holds what the dialog shows and the ids of the ticked keys (never a
// secret: the passphrase lives in the dialog for the time of the call).
import type { ErrorSummary, ImportPreview, ImportReport } from "../api";
import { asError } from "../api";
import { backend } from "../backend";
import { applyRequest, keepSelected, toggleKey } from "../importform";

export type ImportView =
  | { kind: "picking" }
  | { kind: "preview"; preview: ImportPreview; busy: boolean }
  | { kind: "applying"; preview: ImportPreview }
  | { kind: "done"; report: ImportReport }
  | { kind: "error"; error: ErrorSummary };

class Importer {
  open = $state(false);
  view: ImportView = $state({ kind: "picking" });
  /** Prefix every alias with the source's prefix (kept between imports). */
  prefix = $state(false);
  /** Ids of the ticked private keys (none by default, for every new file). */
  selected: string[] = $state([]);
  /** Error of the last action inside the preview (replan, apply). */
  error: ErrorSummary | null = $state(null);
  /** Called after a successful import (the host list reloads). */
  onimported: (() => void) | null = null;
  private run = 0;

  /** Opens the native picker, then shows the plan. */
  async start() {
    const run = ++this.run;
    this.open = true;
    this.error = null;
    this.selected = [];
    this.view = { kind: "picking" };
    try {
      const preview = await backend.importPick(this.prefix);
      if (run !== this.run) return;
      if (!preview) {
        this.open = false;
        return;
      }
      this.view = { kind: "preview", preview, busy: false };
    } catch (e) {
      if (run === this.run) this.view = { kind: "error", error: asError(e) };
    }
  }

  /** Plans the same file again with or without the prefix (ticks kept). */
  async setPrefix(on: boolean) {
    const view = this.view;
    if (view.kind !== "preview" || view.busy) return;
    const run = ++this.run;
    this.prefix = on;
    this.view = { ...view, busy: true };
    try {
      const preview = await backend.importReplan(view.preview.token, on);
      if (run === this.run) {
        this.error = null;
        this.selected = keepSelected(preview.keys, this.selected);
        this.view = { kind: "preview", preview, busy: false };
      }
    } catch (e) {
      if (run !== this.run) return;
      this.prefix = !on;
      this.error = asError(e);
      this.view = { ...view, busy: false };
    }
  }

  toggleKey(id: string) {
    if (this.view.kind !== "preview" || this.view.busy) return;
    this.selected = toggleKey(this.view.preview.keys, this.selected, id);
  }

  /** Writes the plan with the ticked keys. `passphrase` ("" for none) goes
   * to Rust once; the caller clears its field when this resolves. */
  async apply(passphrase: string): Promise<boolean> {
    const view = this.view;
    if (view.kind !== "preview" || view.busy) return false;
    const run = ++this.run;
    this.error = null;
    this.view = { kind: "applying", preview: view.preview };
    const request = applyRequest(view.preview, this.selected, passphrase);
    try {
      const report = await backend.importApply(request);
      if (run !== this.run) return true;
      this.view = { kind: "done", report };
      this.selected = [];
      this.onimported?.();
      return true;
    } catch (e) {
      if (run !== this.run) return false;
      this.error = asError(e);
      this.view = { kind: "preview", preview: view.preview, busy: false };
      return false;
    } finally {
      request.passphrase = null;
    }
  }

  close() {
    ++this.run;
    if (this.view.kind === "preview" || this.view.kind === "picking") void backend.importCancel().catch(() => {});
    this.open = false;
    this.error = null;
    this.selected = [];
    this.view = { kind: "picking" };
  }
}

export const importer = new Importer();
