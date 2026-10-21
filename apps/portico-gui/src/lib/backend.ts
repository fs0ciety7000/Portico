// The engine as seen by the UI: Tauri commands in the desktop app, a local
// simulation in a plain browser (`pnpm dev`, design captures).
import { Channel } from "@tauri-apps/api/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  api,
  type AgentKey,
  type CopyIdOutcome,
  type KeyGenRequest,
  type LocalKey,
  type Snippet,
  type SnippetDraft,
  type VaultStatus,
  isTauri,
  type EngineEvent,
  type ForwardCheck,
  type Host,
  type OnConflict,
  type PromptAnswer,
  type PtySize,
  type RemoteEntry,
  type ThemeInfo,
  type TunnelInfo,
  type Upload,
} from "./api";
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
  openTerminal(session: number, size: PtySize, onData: (bytes: Uint8Array) => void): Promise<number>;
  terminalInput(id: number, data: string, binary: boolean): Promise<void>;
  terminalResize(id: number, cols: number, rows: number, width: number, height: number): Promise<void>;
  terminalAck(id: number, total: number): Promise<void>;
  closeTerminal(id: number): Promise<void>;
  onEngineEvent(cb: (event: EngineEvent) => void): Promise<UnlistenFn>;
  sftpHome(session: number): Promise<string>;
  sftpList(session: number, path: string): Promise<RemoteEntry[]>;
  sftpMkdir(session: number, path: string): Promise<void>;
  sftpRename(session: number, from: string, to: string): Promise<void>;
  sftpRemove(session: number, path: string): Promise<void>;
  pickUploads(): Promise<Upload[]>;
  upload(session: number, token: number, remoteDir: string, onConflict: OnConflict): Promise<number>;
  download(session: number, remotePath: string, onConflict: OnConflict): Promise<number>;
  cancelTransfer(id: number): Promise<void>;
  downloadDir(): Promise<string>;
  chooseDownloadDir(): Promise<string | null>;
  checkForward(spec: string): Promise<ForwardCheck>;
  listTunnels(): Promise<TunnelInfo[]>;
  startTunnel(session: number, spec: string, allowExposed: boolean): Promise<number>;
  stopTunnel(id: number): Promise<void>;
  onDropped(cb: (files: Upload[]) => void): Promise<UnlistenFn>;
  listKeys(): Promise<LocalKey[]>;
  keyDefaults(): Promise<{ comment: string }>;
  generateKey(request: KeyGenRequest): Promise<LocalKey>;
  publicKeyText(key: string): Promise<string>;
  listAgentKeys(): Promise<AgentKey[]>;
  agentAdd(key: string, passphrase: string | null, lifetimeSecs: number | null): Promise<AgentKey>;
  agentRemove(fingerprint: string): Promise<void>;
  copyId(session: number, key: string): Promise<CopyIdOutcome>;
  verifyKeyLogin(session: number, key: string): Promise<void>;
  vaultStatus(): Promise<VaultStatus>;
  vaultCreate(passphrase: string): Promise<void>;
  vaultUnlock(passphrase: string): Promise<void>;
  vaultLock(): Promise<void>;
  vaultChangePassphrase(old: string, next: string): Promise<void>;
  listSnippets(): Promise<Snippet[]>;
  snippetsFor(alias: string): Promise<Snippet[]>;
  saveSnippet(snippet: SnippetDraft): Promise<Snippet>;
  deleteSnippet(id: string): Promise<void>;
  runSnippet(terminal: number, id: string, values: Record<string, string>, bracketedPaste: boolean, run: boolean | null): Promise<void>;
  onDragging(cb: (dragging: boolean) => void): Promise<UnlistenFn>;
}

const tauriBackend: Backend = {
  ...api,
  openTerminal(session, size, onData) {
    const channel = new Channel<ArrayBuffer>();
    channel.onmessage = (buf) => onData(new Uint8Array(buf));
    return api.openTerminal(session, size, channel);
  },
};

/** Outside Tauri: the simulation in development, nothing in a production
 * build (a release page served elsewhere must never pretend to connect). */
const unavailable = new Proxy({} as Backend, {
  get: () => () => Promise.reject({ code: "no_engine", message: "Portico's engine is not available here", hint: null }),
});

export const backend: Backend = isTauri() ? tauriBackend : import.meta.env.DEV || import.meta.env.MODE === "demo" ? demoBackend : unavailable;
