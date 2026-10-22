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
  type HostMeta,
  type ImportPreview,
  type ImportReport,
  type RecentHost,
  type SessionLogInfo,
  type LogLine,
  type EffectiveConfig,
  type Settings,
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
  type PlaybackControl,
  type PlaybackFrame,
  type PlaybackInfo,
  type PlaybackState,
  type Recording,
} from "./api";

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
  setFavorite(alias: string, favorite: boolean): Promise<HostMeta>;
  setTags(alias: string, tags: string[]): Promise<HostMeta>;
  /** Normalized tags, or a `TagsError` (with the offending position). */
  checkTags(tags: string[]): Promise<string[]>;
  setGroup(alias: string, group: string | null): Promise<HostMeta>;
  setNotes(alias: string, notes: string | null): Promise<HostMeta>;
  recentHosts(limit: number): Promise<RecentHost[]>;
  clearRecent(): Promise<void>;
  forgetRecent(alias: string): Promise<boolean>;
  sessionsWithLogs(): Promise<SessionLogInfo[]>;
  sessionLogs(session: number): Promise<LogLine[]>;
  sessionLogsText(session: number): Promise<string>;
  effectiveConfig(alias: string): Promise<EffectiveConfig>;
  getSettings(): Promise<Settings>;
  saveTheme(theme: string): Promise<void>;
  /** Native picker in Rust, then the plan; `null` when cancelled. */
  importTermiusPick(prefix: boolean): Promise<ImportPreview | null>;
  importTermiusReplan(token: number, prefix: boolean): Promise<ImportPreview>;
  importApply(token: number): Promise<ImportReport>;
  importCancel(): Promise<void>;
  recordingStart(terminal: number): Promise<void>;
  recordingStop(terminal: number): Promise<void>;
  recordingStatus(terminal: number): Promise<boolean>;
  recordingsFolder(): Promise<string>;
  listRecordings(): Promise<Recording[]>;
  deleteRecording(name: string): Promise<void>;
  /** Frames (already sanitised) go to `onFrame`, in order. */
  playRecording(name: string, speed: number, onFrame: (frame: PlaybackFrame) => void): Promise<PlaybackInfo>;
  playbackControl(id: number, control: PlaybackControl): Promise<PlaybackState>;
  playbackAck(id: number, total: number): Promise<void>;
  playbackClose(id: number): Promise<void>;
  onEngineLagged(cb: () => void): Promise<UnlistenFn>;
}

const tauriBackend: Backend = {
  ...api,
  openTerminal(session, size, onData) {
    const channel = new Channel<ArrayBuffer>();
    channel.onmessage = (buf) => onData(new Uint8Array(buf));
    return api.openTerminal(session, size, channel);
  },
  playRecording(name, speed, onFrame) {
    const channel = new Channel<PlaybackFrame>();
    channel.onmessage = onFrame;
    return api.playRecording(name, speed, channel);
  },
};

/** Outside Tauri: the simulation in development, nothing in a production
 * build (a release page served elsewhere must never pretend to connect). */
const unavailable = new Proxy({} as Backend, {
  get: (_target, name) =>
    name === "then" || typeof name === "symbol"
      ? undefined
      : () => Promise.reject({ code: "no_engine", message: "Portico's engine is not available here", hint: null }),
});

/** Every method of `Backend` returns a promise: the implementation can load
 * on first use. */
function lazy(load: () => Promise<Backend>): Backend {
  let loaded: Promise<Backend> | null = null;
  return new Proxy({} as Backend, {
    get: (_target, name) =>
      name === "then" || typeof name === "symbol"
        ? undefined
        : (...args: unknown[]) =>
            (loaded ??= load()).then((b) => (b[name as keyof Backend] as (...a: unknown[]) => Promise<unknown>)(...args)),
  });
}

// The demo is a separate chunk, imported only in development or a
// `--mode demo` build: a production bundle neither contains it nor runs its
// side effects (drop listeners). Vite folds this condition at build time.
const demoEnabled = import.meta.env.DEV || import.meta.env.MODE === "demo";

export const backend: Backend = isTauri()
  ? tauriBackend
  : demoEnabled
    ? lazy(() => import("./demo").then((m) => m.installDemo()))
    : unavailable;
