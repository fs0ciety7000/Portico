// Tunnels of every session, derived from engine events (resynchronised from
// `list_tunnels` when the panel opens).
import type { ErrorSummary, TunnelEvent } from "../api";
import { backend } from "../backend";
import { workspace } from "./workspace.svelte";

export type TunnelStatus = "starting" | "listening" | "failed";

export interface Tunnel {
  id: number;
  session: number;
  alias: string;
  display: string;
  exposed: boolean;
  status: TunnelStatus;
  /** Where it listens, once known. */
  bind: string | null;
  connections: number;
  error: ErrorSummary | null;
}

class Tunnels {
  list: Tunnel[] = $state([]);

  /** Engine view wins; failed rows stay until dismissed. */
  async sync() {
    try {
      const known = await backend.listTunnels();
      this.list = [
        ...this.list.filter((t) => t.status === "failed" || known.some((k) => k.id === t.id)),
        ...known
          .filter((k) => !this.list.some((t) => t.id === k.id))
          .map((k) => this.#row(k.id, k.session, k.display, k.exposed)),
      ];
    } catch (e) {
      workspace.notify(e);
    }
  }

  #row(id: number, session: number, display: string, exposed: boolean): Tunnel {
    return {
      id,
      session,
      alias: workspace.aliasOf(session) ?? "",
      display,
      exposed,
      status: "starting",
      bind: null,
      connections: 0,
      error: null,
    };
  }

  /** Returns false when the engine refused it (the error is shown). */
  async start(session: number, spec: string, allowExposed: boolean): Promise<boolean> {
    try {
      const check = await backend.checkForward(spec);
      const id = await backend.startTunnel(session, spec, allowExposed);
      if (!this.list.some((t) => t.id === id)) this.list.push(this.#row(id, session, check.display, check.exposed));
      return true;
    } catch (e) {
      workspace.notify(e);
      return false;
    }
  }

  stop(id: number) {
    const row = this.list.find((t) => t.id === id);
    if (row?.status === "failed") {
      this.list = this.list.filter((t) => t.id !== id);
      return;
    }
    backend.stopTunnel(id).catch(() => {
      this.list = this.list.filter((t) => t.id !== id);
    });
  }

  onEvent(id: number, session: number, ev: TunnelEvent) {
    let row = this.list.find((t) => t.id === id);
    if (!row) {
      if (ev.kind === "stopped") return;
      // Started by an earlier page or before the reply: placeholder until sync.
      this.list.push(this.#row(id, session, "", false));
      row = this.list.find((t) => t.id === id);
      void this.sync();
      if (!row) return;
    }
    switch (ev.kind) {
      case "listening":
        row.status = "listening";
        row.bind = ev.bind;
        return;
      case "connection_opened":
        row.connections++;
        return;
      case "connection_closed":
        row.connections = Math.max(0, row.connections - 1);
        return;
      case "failed":
        row.status = "failed";
        row.error = ev.error;
        return;
      case "stopped":
        // A failure stays visible (with its hint) until dismissed.
        if (row.status !== "failed") this.list = this.list.filter((t) => t.id !== id);
        return;
    }
  }
}

export const tunnels = new Tunnels();
