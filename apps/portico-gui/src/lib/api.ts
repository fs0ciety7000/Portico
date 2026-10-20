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

export const isTauri = (): boolean => "__TAURI_INTERNALS__" in window;

export const api = {
  listHosts: () => invoke<Host[]>("list_hosts"),
  searchHosts: (query: string) => invoke<Host[]>("search_hosts", { query }),
  listThemes: () => invoke<ThemeInfo[]>("list_themes"),
  connect: (alias: string) => invoke<number>("connect", { alias }),
  onEngineEvent: (cb: (event: unknown) => void): Promise<UnlistenFn> =>
    listen("engine://event", (e) => cb(e.payload)),
};

export function asError(e: unknown): ErrorSummary {
  if (e && typeof e === "object" && "message" in e) return e as ErrorSummary;
  return { code: "unknown", message: String(e), hint: null };
}
