// Typed bridge to the Rust commands (apps/portico-gui/src-tauri/src/commands.rs).
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface Host {
  alias: string;
  hostname: string;
  user: string | null;
  port: number;
  proxyJump: string[];
  identityFiles: string[];
  localForwards: string[];
  remoteForwards: string[];
  tags: string[];
  group: string | null;
  favorite: boolean;
  source: string;
  warnings: string[];
}

export interface ThemeInfo {
  id: string;
  name: string;
  description: string;
}

/** Mirrors `portico_core::ErrorSummary`. */
export interface ErrorSummary {
  code: string;
  message: string;
  hint: string | null;
}

export type Phase =
  | { phase: "resolving" }
  | { phase: "connecting"; hop: number; hops: number; target: string }
  | { phase: "handshake" }
  | { phase: "verifying_host_key" }
  | { phase: "authenticating"; method: string }
  | { phase: "connected" }
  | { phase: "disconnecting" }
  | { phase: "closed"; reason: string }
  | { phase: "failed"; error: ErrorSummary };

export interface Duration {
  secs: number;
  nanos: number;
}

/** Mirrors `portico_core::events::EngineEvent` (only the variants the UI uses). */
export type EngineEvent =
  | {
      type: "session";
      id: number;
      event:
        | ({ kind: "phase" } & Phase)
        | { kind: "latency"; rtt: Duration }
        | { kind: "started"; alias: string }
        | { kind: "banner"; text: string }
        | { kind: "host_key_changed"; host: string; fingerprint: string; expected_fingerprint: string };
    }
  | { type: "prompt"; id: number; session: number | null; kind: { kind: string } }
  | { type: "prompt_resolved"; id: number }
  | { type: string };

export type PromptAnswer =
  | { kind: "accept_host_key" }
  | { kind: "secret"; value: string; remember: boolean }
  | { kind: "answers"; values: string[] }
  | { kind: "cancel" };

export const isTauri = (): boolean => "__TAURI_INTERNALS__" in window;

export const api = {
  listHosts: () => invoke<Host[]>("list_hosts"),
  searchHosts: (query: string) => invoke<Host[]>("search_hosts", { query }),
  listThemes: () => invoke<ThemeInfo[]>("list_themes"),
  connect: (alias: string) => invoke<number>("connect", { alias }),
  disconnect: (session: number) => invoke<void>("disconnect", { session }),
  respondPrompt: (prompt: number, answer: PromptAnswer) => invoke<void>("respond_prompt", { prompt, answer }),
  onEngineEvent: (cb: (event: EngineEvent) => void): Promise<UnlistenFn> =>
    listen<EngineEvent>("engine://event", (e) => cb(e.payload)),
};

export function asError(e: unknown): ErrorSummary {
  if (e && typeof e === "object" && "message" in e) return e as ErrorSummary;
  return { code: "unknown", message: String(e), hint: null };
}

export type Status = "idle" | "connecting" | "authenticating" | "connected" | "warning" | "error";

/** Live view of a session, derived from engine events. */
export interface SessionView {
  id: number;
  phase: Phase;
  latencyMs: number | null;
}

const SLOW_RTT_MS = 250;

/** Phase → badge status + label (same vocabulary as the TUI). */
export function describe(view: SessionView | undefined): { status: Status; label: string; busy: boolean } {
  if (!view) return { status: "idle", label: "Disconnected", busy: false };
  const p = view.phase;
  switch (p.phase) {
    case "resolving":
      return { status: "connecting", label: "Resolving", busy: true };
    case "connecting":
      return {
        status: "connecting",
        label: p.hops > 1 ? `Connecting · ${p.target} (${p.hop}/${p.hops})` : "Connecting",
        busy: true,
      };
    case "handshake":
      return { status: "connecting", label: "Negotiating", busy: true };
    case "verifying_host_key":
      return { status: "authenticating", label: "Verifying host key", busy: true };
    case "authenticating":
      return { status: "authenticating", label: `Authenticating · ${p.method.replace("_", " ")}`, busy: true };
    case "connected":
      return {
        status: view.latencyMs !== null && view.latencyMs > SLOW_RTT_MS ? "warning" : "connected",
        label: "Connected",
        busy: false,
      };
    case "disconnecting":
      return { status: "idle", label: "Disconnecting", busy: true };
    case "failed":
      return { status: "error", label: `Failed · ${p.error.code.replaceAll("_", " ")}`, busy: false };
    default:
      return { status: "idle", label: "Disconnected", busy: false };
  }
}

/** `<1 ms`, `4.2 ms`, `38 ms`. */
export function formatRtt(ms: number): string {
  if (ms < 1) return "<1 ms";
  if (ms < 10) return `${ms.toFixed(1)} ms`;
  return `${Math.round(ms)} ms`;
}
