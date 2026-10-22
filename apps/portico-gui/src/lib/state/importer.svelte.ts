// "Import from Termius…": native picker (Rust), preview of the plan, then
// the import and its report. The plan stays in Rust under a token; this
// state only holds what the dialog shows (never a secret: counts only).
import type { ErrorSummary, ImportPreview, ImportReport } from "../api";
import { asError } from "../api";
import { backend } from "../backend";

export type ImportView =
  | { kind: "picking" }
  | { kind: "preview"; preview: ImportPreview; busy: boolean }
  | { kind: "applying"; preview: ImportPreview }
  | { kind: "done"; report: ImportReport }
  | { kind: "error"; error: ErrorSummary };

class Importer {
  open = $state(false);
  view: ImportView = $state({ kind: "picking" });
  /** Prefix every alias with `termius-` (kept between imports). */
  prefix = $state(false);
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
    this.view = { kind: "picking" };
    try {
      const preview = await backend.importTermiusPick(this.prefix);
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

  /** Plans the same file again with or without the prefix. */
  async setPrefix(on: boolean) {
    const view = this.view;
    if (view.kind !== "preview" || view.busy) return;
    const run = ++this.run;
    this.prefix = on;
    this.view = { ...view, busy: true };
    try {
      const preview = await backend.importTermiusReplan(view.preview.token, on);
      if (run === this.run) {
        this.error = null;
        this.view = { kind: "preview", preview, busy: false };
      }
    } catch (e) {
      if (run !== this.run) return;
      this.prefix = !on;
      this.error = asError(e);
      this.view = { ...view, busy: false };
    }
  }

  async apply() {
    const view = this.view;
    if (view.kind !== "preview" || view.busy) return;
    const run = ++this.run;
    this.error = null;
    this.view = { kind: "applying", preview: view.preview };
    try {
      const report = await backend.importApply(view.preview.token);
      if (run !== this.run) return;
      this.view = { kind: "done", report };
      this.onimported?.();
    } catch (e) {
      if (run !== this.run) return;
      this.error = asError(e);
      this.view = { kind: "preview", preview: view.preview, busy: false };
    }
  }

  close() {
    ++this.run;
    if (this.view.kind === "preview" || this.view.kind === "picking") void backend.importCancel().catch(() => {});
    this.open = false;
    this.error = null;
    this.view = { kind: "picking" };
  }
}

export const importer = new Importer();
