// The engine as seen by the UI: Tauri commands in the desktop app, a local
// simulation in a plain browser (`pnpm dev`, design captures).
import { Channel } from "@tauri-apps/api/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { api, isTauri, type EngineEvent, type Host, type PromptAnswer, type ThemeInfo } from "./api";
import { demoBackend } from "./demo";

export interface Backend {
  listHosts(): Promise<Host[]>;
  searchHosts(query: string): Promise<Host[]>;
  listThemes(): Promise<ThemeInfo[]>;
  homeDir(): Promise<string | null>;
  connect(alias: string): Promise<number>;
  disconnect(session: number): Promise<void>;
  respondPrompt(prompt: number, answer: PromptAnswer): Promise<void>;
  /** Resolves with the terminal id once the PTY is open; output goes to `onData`. */
  openTerminal(session: number, cols: number, rows: number, onData: (bytes: Uint8Array) => void): Promise<number>;
  terminalInput(id: number, data: string, binary: boolean): Promise<void>;
  terminalResize(id: number, cols: number, rows: number): Promise<void>;
  terminalAck(id: number, total: number): Promise<void>;
  closeTerminal(id: number): Promise<void>;
  onEngineEvent(cb: (event: EngineEvent) => void): Promise<UnlistenFn>;
}

const tauriBackend: Backend = {
  ...api,
  openTerminal(session, cols, rows, onData) {
    const channel = new Channel<ArrayBuffer>();
    channel.onmessage = (buf) => onData(new Uint8Array(buf));
    return api.openTerminal(session, cols, rows, channel);
  },
};

export const backend: Backend = isTauri() ? tauriBackend : demoBackend;
