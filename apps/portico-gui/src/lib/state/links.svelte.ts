// The link waiting for "Open this link?" (phase 9, ADR "Liens du terminal").
// A terminal pane asks with `request` after a ⌘-click / Ctrl-click; nothing
// opens without the dialog's answer, and the Rust command checks again.
import { backend } from "../backend";
import { checkLink, textMisleads, TOO_LONG } from "../links";
import { workspace } from "./workspace.svelte";

export interface PendingLink {
  href: string;
  host: string;
  /** OSC 8 visible text that names something else than the host, else null. */
  misleading: string | null;
}

class LinkState {
  pending: PendingLink | null = $state(null);

  /** `text`: the visible text of an OSC 8 hyperlink (null for a plain URL). */
  request(raw: string, text: string | null) {
    const link = checkLink(raw);
    if (!link.ok) {
      if (link.reason === "too_long") workspace.notify({ code: "link_too_long", message: TOO_LONG, hint: null }, "warning");
      else
        workspace.notify(
          {
            code: "link_refused",
            message: "This link cannot be opened",
            hint: "Only http and https links without a user name or password open from Portico.",
          },
          "warning",
        );
      return;
    }
    this.pending = { href: link.href, host: link.host, misleading: textMisleads(text, link.href, link.host) ? text : null };
  }

  cancel() {
    this.pending = null;
  }

  async open() {
    const link = this.pending;
    this.pending = null;
    if (!link) return;
    try {
      await backend.openUrl(link.href);
    } catch (e) {
      workspace.notify(e, "warning");
    }
  }
}

export const links = new LinkState();
