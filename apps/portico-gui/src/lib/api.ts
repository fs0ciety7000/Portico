// Typed bridge to the Rust commands (apps/portico-gui/src-tauri/src/commands.rs).
import { Channel, invoke } from "@tauri-apps/api/core";
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
  /** `DynamicForward` values (`-D`, SOCKS5). */
  dynamicForwards: string[];
  tags: string[];
  group: string | null;
  favorite: boolean;
  /** Free text from hosts.toml (`\n` allowed, nothing else invisible). */
  notes: string | null;
  source: string;
  /** Which file declares the host: only `portico` hosts are editable. */
  origin: HostOrigin;
  /** A Portico host's own directives in hosts.conf (null otherwise). */
  managed: ManagedFields | null;
  warnings: string[];
}

/** Mirrors `portico_core::hosts::HostOrigin`. */
export type HostOrigin = "ssh_config" | "portico" | "imported";

/** Mirrors `dto::ManagedFieldsDto`: what hosts.conf says, nothing inherited. */
export interface ManagedFields {
  hostname: string;
  user: string | null;
  port: number | null;
  /** `~/…`, display and preselection only: never sent back. */
  identityFile: string | null;
  proxyJump: string | null;
}

/** Mirrors `hosts::IdentityChoiceDto`: a discovered key by id, never a path. */
export type IdentityChoice = { kind: "none" } | { kind: "keep" } | { kind: "key"; id: string };

/** Mirrors `hosts::PasswordChangeDto`: `value` becomes a SecretString in Rust. */
export type PasswordChange = { kind: "keep" } | { kind: "set"; value: string } | { kind: "forget" };

/** Mirrors `hosts::HostDraftDto` (validated by the engine). */
export interface HostDraft {
  alias: string;
  hostname: string;
  user: string | null;
  port: number | null;
  identity: IdentityChoice;
  proxyJump: string | null;
  tags: string[];
  group: string | null;
  favorite: boolean;
  notes: string | null;
  password: PasswordChange;
}

/** Mirrors `hosts::ManagedIncludeDto`. */
export interface ManagedInclude {
  line: string;
  active: boolean;
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

/** Mirrors `meta::TagsErrorDto`: a refused tag list, with the 1-based
 * position of the offending tag when the core names one. */
export interface TagsError extends ErrorSummary {
  position: number | null;
}

/** The offending tag's 1-based position carried by a tags error, if any. */
export function tagPosition(e: unknown): number | null {
  const p = e && typeof e === "object" && "position" in e ? (e as { position: unknown }).position : null;
  return typeof p === "number" && Number.isInteger(p) && p > 0 ? p : null;
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

/** Mirrors `portico_core::events::PromptKind`. */
export type PromptKind =
  | { kind: "unknown_host_key"; host: string; port: number; algorithm: string; fingerprint: string; other_algorithms: string[] }
  | { kind: "password"; user: string; host: string; attempt: number; attempts: number }
  | { kind: "key_passphrase"; key_path: string; attempt: number; attempts: number }
  | { kind: "keyboard_interactive"; name: string; instructions: string; prompts: { text: string; echo: boolean }[] };

export interface Prompt {
  id: number;
  session: number | null;
  kind: PromptKind;
}

export interface HostKeyChanged {
  host: string;
  port: number;
  algorithm: string;
  fingerprint: string;
  expected_fingerprint: string;
  known_hosts_file: string;
  known_hosts_line: number;
}

export type SessionEvent =
  | ({ kind: "phase" } & Phase)
  | { kind: "latency"; rtt: Duration }
  /** `probe`: a short-lived check (copy-id verification), not a session the
   * user opened: never listed as the host's session. */
  | { kind: "started"; alias: string; probe?: boolean }
  | { kind: "banner"; text: string }
  | ({ kind: "host_key_changed" } & HostKeyChanged);

export type TerminalEvent =
  | { kind: "opened" }
  | { kind: "exited"; status: number | null; signal: string | null }
  | { kind: "closed" };

export type Direction = "upload" | "download";

/** Mirrors `portico_core::events::SftpEvent`. */
export type SftpEvent =
  | { kind: "ready"; home: string }
  | { kind: "transfer_started"; transfer: number; direction: Direction; name: string; total: number | null }
  | { kind: "transfer_progress"; transfer: number; done: number; total: number | null }
  | { kind: "transfer_finished"; transfer: number; destination: string }
  | { kind: "transfer_skipped"; transfer: number }
  | { kind: "transfer_cancelled"; transfer: number }
  | { kind: "transfer_failed"; transfer: number; error: ErrorSummary }
  | { kind: "closed" };

/** Mirrors `portico_core::events::TunnelEvent`. */
export type TunnelEvent =
  | { kind: "listening"; bind: string }
  | { kind: "connection_opened"; peer: string }
  | { kind: "connection_closed"; peer: string }
  | { kind: "stopped" }
  | { kind: "failed"; error: ErrorSummary };

/** Mirrors `portico_core::events::EngineEvent` (only the variants the UI uses). */
export type EngineEvent =
  | { type: "session"; id: number; event: SessionEvent }
  | { type: "terminal"; id: number; session: number; event: TerminalEvent }
  | ({ type: "prompt" } & Prompt)
  | { type: "prompt_resolved"; id: number }
  | { type: "sftp"; id: number; session: number; event: SftpEvent }
  | { type: "tunnel"; id: number; session: number; event: TunnelEvent }
  /** A log record was added to `session` (display it via `sessionLogs`, which is sanitized). */
  | { type: "log"; session: number | null }
  /** `hosts.toml` changed for `alias`: update the host in place. */
  | { type: "host_meta"; alias: string; meta: HostMeta }
  /** Portico's hosts.conf changed (created, edited, deleted): reload the inventory. */
  | { type: "hosts_changed" }
  /** The recent-sessions list changed: fetch it again if shown. */
  | { type: "recent_changed" }
  /** A terminal recording started or stopped: `REC` shows between the two. */
  | { type: "recording"; terminal: number; session: number; state: RecordingState };

/** Mirrors `portico_core::recording::RecordingStopReason`. */
export type RecordingStopReason = "requested" | "terminal_closed" | "queue_full" | "write_failed" | "size_limit";

/** Mirrors `portico_core::events::RecordingState`. The view never sends
 * `path` back (requests name recordings by `name` only) nor shows it. */
export type RecordingState =
  | { state: "started"; name: string; path: string }
  | { state: "stopped"; reason: RecordingStopReason; name: string; path: string; bytes: number; duration: Duration };

/** Mirrors `portico_core::hosts::HostMeta` (as serialized in events). */
export interface HostMeta {
  tags: string[];
  group: string | null;
  favorite: boolean;
  notes: string | null;
}

/** Mirrors `meta::RecentDto`; `last` in Unix ms. */
export interface RecentHost {
  alias: string;
  last: number;
  count: number;
}

export type CloseReason = "user_requested" | "remote_closed" | "timeout";

/** Mirrors `debug::SessionLogDto` (times in Unix ms; texts escaped in Rust). */
export interface SessionLogInfo {
  id: number;
  alias: string;
  probe: boolean;
  started: number;
  ended: number | null;
  outcome: { kind: "closed"; reason: CloseReason } | { kind: "failed"; error: ErrorSummary } | null;
}

export type LogLevel = "trace" | "debug" | "info" | "warn" | "error";

/** Mirrors `debug::LogLineDto`: one line, controls and bidi escaped. */
export interface LogLine {
  at: number;
  level: LogLevel;
  target: string;
  message: string;
}

/** Mirrors `debug::EffectiveOptionDto`. `origin` null = Portico's default. */
export interface EffectiveOption {
  key: string;
  values: string[];
  origin: string | null;
  command: boolean;
  /** Runs commands (ProxyCommand…): values withheld by Rust, never shown or copied. */
  sensitive: boolean;
}

/** Mirrors `debug::EffectiveConfigDto` (the `ssh -G` view). */
export interface EffectiveConfig {
  alias: string;
  options: EffectiveOption[];
  route: { alias: string; user: string; hostname: string; port: number }[];
  routeError: ErrorSummary | null;
  warnings: { origin: string; message: string }[];
}

/** Mirrors `settings::SettingsDto` (GUI part of settings.toml). */
export interface Settings {
  theme: string | null;
  keymap: Record<string, string>;
  error: ErrorSummary | null;
}

export type EntryKind = "file" | "dir" | "symlink" | "other";

/** Mirrors `files::EntryDto`. Show `display`; `name` is raw (operations). */
export interface RemoteEntry {
  name: string;
  display: string;
  path: string;
  kind: EntryKind;
  size: number;
  mode: number | null;
  /** Unix seconds. */
  modified: number | null;
}

/** A local file handed over by the native picker or a drop (`files::UploadDto`). */
export interface Upload {
  token: number;
  display: string;
  size: number;
}

export type OnConflict = "ask" | "overwrite" | "skip" | "rename";

/** Mirrors `files::ForwardDto`. */
export interface ForwardCheck {
  display: string;
  exposed: boolean;
  /** A SOCKS proxy (`-D`): exposed, it is an open relay. */
  dynamic: boolean;
}

/** Mirrors `files::TunnelDto`. */
export interface TunnelInfo {
  id: number;
  session: number;
  display: string;
  exposed: boolean;
}

/** Mirrors `dto::PtySizeDto`: cells and pixels. */
export interface PtySize {
  cols: number;
  rows: number;
  width: number;
  height: number;
}

export type PromptAnswer =
  | { kind: "accept_host_key" }
  | { kind: "secret"; value: string; remember: boolean }
  | { kind: "answers"; values: string[] }
  | { kind: "cancel" };

/** Mirrors `portico_core::keys::CertValidity`. */
export type CertValidity =
  | { state: "not_yet_valid"; starts_in_secs: number }
  | { state: "valid"; expires_in_secs: number | null }
  | { state: "expires_soon"; expires_in_secs: number }
  | { state: "expired"; since_secs: number };

/** Mirrors `keys::CertificateDto`. */
export interface Certificate {
  keyId: string;
  principals: string[];
  /** Unix seconds. */
  validAfter: number;
  /** Unix seconds; null = forever. */
  validBefore: number | null;
  validity: CertValidity;
}

/** Mirrors `keys::LocalKeyDto`. `id` (the file name) designates the key. */
export interface LocalKey {
  id: string;
  privateDisplay: string;
  publicDisplay: string;
  algorithm: string;
  fingerprint: string;
  comment: string;
  hasPrivate: boolean;
  certificate: Certificate | null;
}

/** Mirrors `keys::AgentKeyDto`. */
export interface AgentKey {
  algorithm: string;
  fingerprint: string;
  comment: string;
  certificate: Certificate | null;
}

/** Mirrors `portico_core::keys::KeyAlgorithm`. */
export type KeyAlgorithm =
  | { kind: "ed25519" }
  | { kind: "rsa"; bits: number }
  | { kind: "ecdsa"; curve: "p256" | "p384" | "p521" };

/** Sent once and forgotten: the passphrase becomes a SecretString in Rust. */
export interface KeyGenRequest {
  name: string;
  algorithm: KeyAlgorithm;
  comment: string;
  passphrase: string | null;
}

export type CopyIdOutcome = "added" | "already_present";

/** Mirrors `portico_core::vault::VaultStatus`. */
export type VaultStatus = "not_in_use" | "absent" | "locked" | "unlocked";

/** Mirrors `portico_core::snippets::SnippetScope`. */
export type SnippetScope = { kind: "global" } | { kind: "hosts"; values: string[] } | { kind: "tags"; values: string[] };

/** Mirrors `snippets::SnippetDto` (`placeholders` is read-only, from Rust). */
export interface Snippet {
  id: string;
  name: string;
  command: string;
  description: string | null;
  scope: SnippetScope;
  run: boolean;
  placeholders: string[];
}

/** What `save_snippet` takes (an empty id creates). */
export type SnippetDraft = Omit<Snippet, "placeholders">;

/** Mirrors `recordings::RecordingDto`. `name` is the only id sent back
 * (play, delete); `alias` comes from the file header (escaped in Rust). */
export interface Recording {
  name: string;
  alias: string;
  /** Unix ms. */
  started: number | null;
  size: number;
  /** Seconds. */
  duration: number | null;
  width: number | null;
  height: number | null;
  valid: boolean;
}

/** Mirrors `recordings::PlaybackDto`. */
export interface PlaybackInfo {
  id: number;
  name: string;
  title: string | null;
  cols: number;
  rows: number;
  paused: boolean;
  speed: number;
}

/** Mirrors `recordings::PlaybackStateDto`. */
export interface PlaybackState {
  paused: boolean;
  speed: number;
}

/** Mirrors `recordings::PlaybackControlDto`. */
export type PlaybackControl =
  | { kind: "pause" }
  | { kind: "resume" }
  | { kind: "speed"; value: number }
  | { kind: "skip" }
  | { kind: "seek"; time: number };

/** Mirrors `recordings::PlaybackFrame`: output is already sanitised in Rust. */
export type PlaybackFrame =
  | { kind: "output"; data: string; size: number; time: number }
  | { kind: "resize"; cols: number; rows: number; time: number }
  | { kind: "reset"; time: number }
  | { kind: "end"; time: number }
  | { kind: "error"; error: ErrorSummary };

export const isTauri = (): boolean => "__TAURI_INTERNALS__" in window;

/** Mirrors `portico_core::import::ImportNotice` (never a secret). */
export interface ImportNotice {
  /** `unsupported_protocol`, `missing_hostname`, `invalid_hostname`, `alias_collision`, `tag_normalized`… */
  code: string;
  record: number | null;
  label: string | null;
  message: string;
}

/** Mirrors `portico_core::import::PlannedHost`. */
export interface PlannedHost {
  alias: string;
  label: string;
  hostname: string;
  user: string | null;
  /** `null` for 22. */
  port: number | null;
  group: string | null;
  tags: string[];
  notes: string | null;
  record: number;
}

/** Secrets found in the export and ignored (counts only). */
export interface IgnoredSecrets {
  passwords: number;
  keys: number;
}

/** Mirrors `import::ImportPreviewDto`: a plan kept in Rust under `token`. */
export interface ImportPreview {
  token: number;
  /** File name only: the path stays in Rust. */
  fileName: string;
  format: string;
  records: number;
  prefix: boolean;
  hosts: PlannedHost[];
  skipped: ImportNotice[];
  warnings: ImportNotice[];
  secrets: IgnoredSecrets;
  secretsAdvice: string | null;
  identityFiles: string[];
  target: string;
  includeLine: string;
  /** `Host` blocks already in `target` (a previous import) that this one replaces. */
  replaces: number | null;
}

/** Mirrors `import::ImportReportDto` (`portico_core::import::ImportReport`). */
export interface ImportReport {
  formatLabel: string;
  target: string;
  unchanged: boolean;
  imported: string[];
  skipped: ImportNotice[];
  warnings: ImportNotice[];
  secrets: IgnoredSecrets;
  secretsAdvice: string | null;
  identityFiles: string[];
  metaUpdated: number;
  includeLine: string;
  includeActive: boolean;
}

export const api = {
  listHosts: () => invoke<Host[]>("list_hosts"),
  searchHosts: (query: string) => invoke<Host[]>("search_hosts", { query }),
  listThemes: () => invoke<ThemeInfo[]>("list_themes"),
  homeDir: () => invoke<string | null>("home_dir"),
  connect: (alias: string) => invoke<number>("connect", { alias }),
  disconnect: (session: number) => invoke<void>("disconnect", { session }),
  respondPrompt: (prompt: number, answer: PromptAnswer) => invoke<void>("respond_prompt", { prompt, answer }),
  /** Opens a PTY (waits for the session); raw output arrives on `output`. */
  openTerminal: (session: number, size: PtySize, output: Channel<ArrayBuffer>) =>
    invoke<number>("open_terminal", { session, size, output }),
  terminalInput: (id: number, data: string, binary: boolean) => invoke<void>("terminal_input", { id, data, binary }),
  terminalResize: (id: number, cols: number, rows: number, width: number, height: number) =>
    invoke<void>("terminal_resize", { id, size: { cols, rows, width, height } }),
  /** Flow control: `total` bytes of output have been parsed by xterm.js. */
  terminalAck: (id: number, total: number) => invoke<void>("terminal_ack", { id, total }),
  closeTerminal: (id: number) => invoke<void>("close_terminal", { id }),
  onEngineEvent: (cb: (event: EngineEvent) => void): Promise<UnlistenFn> =>
    listen<EngineEvent>("engine://event", (e) => cb(e.payload)),

  sftpHome: (session: number) => invoke<string>("sftp_home", { session }),
  sftpList: (session: number, path: string) => invoke<RemoteEntry[]>("sftp_list", { session, path }),
  sftpMkdir: (session: number, path: string) => invoke<void>("sftp_mkdir", { session, path }),
  sftpRename: (session: number, from: string, to: string) => invoke<void>("sftp_rename", { session, from, to }),
  sftpRemove: (session: number, path: string) => invoke<void>("sftp_remove", { session, path }),
  /** Native file picker (Rust side); the files come back as tokens. */
  pickUploads: () => invoke<Upload[]>("pick_uploads"),
  upload: (session: number, token: number, remoteDir: string, onConflict: OnConflict) =>
    invoke<number>("upload", { session, token, remoteDir, onConflict }),
  /** Into the download folder, under the remote name. */
  download: (session: number, remotePath: string, onConflict: OnConflict) =>
    invoke<number>("download", { session, remotePath, onConflict }),
  cancelTransfer: (id: number) => invoke<void>("cancel_transfer", { id }),
  downloadDir: () => invoke<string>("download_dir"),
  chooseDownloadDir: () => invoke<string | null>("choose_download_dir"),
  checkForward: (spec: string) => invoke<ForwardCheck>("check_forward", { spec }),
  listTunnels: () => invoke<TunnelInfo[]>("list_tunnels"),
  startTunnel: (session: number, spec: string, allowExposed: boolean) =>
    invoke<number>("start_tunnel", { session, spec, allowExposed }),
  stopTunnel: (id: number) => invoke<void>("stop_tunnel", { id }),
  listKeys: () => invoke<LocalKey[]>("list_keys"),
  keyDefaults: () => invoke<{ comment: string }>("key_defaults"),
  generateKey: (request: KeyGenRequest) => invoke<LocalKey>("generate_key", { request }),
  publicKeyText: (key: string) => invoke<string>("public_key_text", { key }),
  listAgentKeys: () => invoke<AgentKey[]>("list_agent_keys"),
  agentAdd: (key: string, passphrase: string | null, lifetimeSecs: number | null) =>
    invoke<AgentKey>("agent_add", { key, passphrase, lifetimeSecs }),
  agentRemove: (fingerprint: string) => invoke<void>("agent_remove", { fingerprint }),
  copyId: (session: number, key: string) => invoke<CopyIdOutcome>("copy_id", { session, key }),
  verifyKeyLogin: (session: number, key: string) => invoke<void>("verify_key_login", { session, key }),
  vaultStatus: () => invoke<VaultStatus>("vault_status"),
  vaultCreate: (passphrase: string) => invoke<void>("vault_create", { passphrase }),
  vaultUnlock: (passphrase: string) => invoke<void>("vault_unlock", { passphrase }),
  vaultLock: () => invoke<void>("vault_lock"),
  vaultChangePassphrase: (old: string, next: string) => invoke<void>("vault_change_passphrase", { old, new: next }),
  listSnippets: () => invoke<Snippet[]>("list_snippets"),
  snippetsFor: (alias: string) => invoke<Snippet[]>("snippets_for", { alias }),
  saveSnippet: (snippet: SnippetDraft) => invoke<Snippet>("save_snippet", { snippet }),
  deleteSnippet: (id: string) => invoke<void>("delete_snippet", { id }),
  /** Rendered and checked in Rust; `run` overrides the snippet's flag. */
  runSnippet: (terminal: number, id: string, values: Record<string, string>, bracketedPaste: boolean, run: boolean | null) =>
    invoke<void>("run_snippet", { terminal, id, values, bracketedPaste, run }),
  setFavorite: (alias: string, favorite: boolean) => invoke<HostMeta>("set_favorite", { alias, favorite }),
  setTags: (alias: string, tags: string[]) => invoke<HostMeta>("set_tags", { alias, tags }),
  checkTags: (tags: string[]) => invoke<string[]>("check_tags", { tags }),
  setGroup: (alias: string, group: string | null) => invoke<HostMeta>("set_group", { alias, group }),
  setNotes: (alias: string, notes: string | null) => invoke<HostMeta>("set_notes", { alias, notes }),
  /** Returns the alias as written (the engine validates everything). */
  createHost: (draft: HostDraft) => invoke<string>("create_host", { draft }),
  /** Returns the new alias (a rename included). */
  updateHost: (alias: string, draft: HostDraft) => invoke<string>("update_host", { alias, draft }),
  deleteHost: (alias: string) => invoke<void>("delete_host", { alias }),
  duplicateHost: (alias: string, newAlias: string) => invoke<string>("duplicate_host", { alias, newAlias }),
  managedInclude: () => invoke<ManagedInclude>("managed_include"),
  recentHosts: (limit: number) => invoke<RecentHost[]>("recent_hosts", { limit }),
  clearRecent: () => invoke<void>("clear_recent"),
  forgetRecent: (alias: string) => invoke<boolean>("forget_recent", { alias }),
  sessionsWithLogs: () => invoke<SessionLogInfo[]>("sessions_with_logs"),
  sessionLogs: (session: number) => invoke<LogLine[]>("session_logs", { session }),
  /** "Copy all": `LogRecord::to_line`, one record per line. */
  sessionLogsText: (session: number) => invoke<string>("session_logs_text", { session }),
  effectiveConfig: (alias: string) => invoke<EffectiveConfig>("effective_config", { alias }),
  getSettings: () => invoke<Settings>("get_settings"),
  saveTheme: (theme: string) => invoke<void>("save_theme", { theme }),
  /** Native file picker (Rust side), then the plan; `null` when cancelled. */
  importTermiusPick: (prefix: boolean) => invoke<ImportPreview | null>("import_termius_pick", { prefix }),
  importTermiusReplan: (token: number, prefix: boolean) => invoke<ImportPreview>("import_termius_replan", { token, prefix }),
  importApply: (token: number) => invoke<ImportReport>("import_apply", { token }),
  importCancel: () => invoke<void>("import_cancel"),
  /** Starts recording a terminal; `REC` shows from the `started` event only. */
  recordingStart: (terminal: number) => invoke<void>("recording_start", { terminal }),
  recordingStop: (terminal: number) => invoke<void>("recording_stop", { terminal }),
  /** Resync after a lagged event stream only. */
  recordingStatus: (terminal: number) => invoke<boolean>("recording_status", { terminal }),
  /** The recordings folder for display (`~/…`), built in Rust. */
  recordingsFolder: () => invoke<string>("recordings_folder"),
  listRecordings: () => invoke<Recording[]>("list_recordings"),
  /** `name` from `listRecordings` only (validated again in Rust). */
  deleteRecording: (name: string) => invoke<void>("delete_recording", { name }),
  playRecording: (name: string, speed: number, frames: Channel<PlaybackFrame>) =>
    invoke<PlaybackInfo>("play_recording", { name, speed, frames }),
  playbackControl: (id: number, control: PlaybackControl) => invoke<PlaybackState>("playback_control", { id, control }),
  /** Flow control: `total` bytes of playback output were parsed by the player. */
  playbackAck: (id: number, total: number) => invoke<void>("playback_ack", { id, total }),
  playbackClose: (id: number) => invoke<void>("playback_close", { id }),
  /** The engine event stream lost events: resync what is derived from them. */
  onEngineLagged: (cb: () => void): Promise<UnlistenFn> => listen<number>("portico://engine-lagged", () => cb()),
  /** Files dropped on the window, already registered as upload tokens. */
  onDropped: (cb: (files: Upload[]) => void): Promise<UnlistenFn> =>
    listen<Upload[]>("portico://dropped", (e) => cb(e.payload)),
  onDragging: (cb: (dragging: boolean) => void): Promise<UnlistenFn> =>
    listen<boolean>("portico://dragging", (e) => cb(e.payload)),
};

let home: string | null = null;

/** Called once at startup; `displayPath` then shortens home paths to `~`. */
export function setHome(dir: string | null) {
  home = dir ? dir.replace(/\/+$/, "") : null;
}

export function displayPath(path: string): string {
  return home && (path === home || path.startsWith(`${home}/`)) ? `~${path.slice(home.length)}` : path;
}

export function durationMs(d: Duration): number {
  return d.secs * 1000 + d.nanos / 1e6;
}

export function asError(e: unknown): ErrorSummary {
  // A native `Error` / `DOMException` (clipboard, IPC) keeps `name` and
  // `message` on its prototype chain: a spread would lose them.
  if (e instanceof Error || (typeof DOMException !== "undefined" && e instanceof DOMException)) {
    return { code: e.name || "error", message: e.message || String(e), hint: null };
  }
  if (e && typeof e === "object" && "message" in e) {
    const { code, message, hint } = e as Partial<ErrorSummary>;
    return { code: typeof code === "string" ? code : "unknown", message: String(message ?? ""), hint: typeof hint === "string" ? hint : null };
  }
  return { code: "unknown", message: String(e), hint: null };
}

/** Core messages are lowercase sentences (`wrong vault passphrase`): the UI
 * capitalizes the first word for display, unless it is a name, a path or an
 * identifier (`id_ed25519 …`, `~/.ssh/…`, `ssh-agent …`), left as written. */
export function sentence(text: string): string {
  return /^[a-z]+(?:\s|$)/.test(text) ? text.charAt(0).toUpperCase() + text.slice(1) : text;
}

/** A core message cut at its backticks: `code` parts are shown in mono
 * (`tag \`Web Serveur\` imported as \`web-serveur\``). An unpaired
 * backtick is kept as text. */
export function codeSpans(text: string): { text: string; code: boolean }[] {
  const parts = text.split("`");
  if (parts.length % 2 === 0) return [{ text, code: false }];
  return parts.map((part, i) => ({ text: part, code: i % 2 === 1 })).filter((p) => p.text !== "");
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
      // A refusal the user chose is not an error.
      if (p.error.code === "host_key_rejected") return { status: "idle", label: "Refused", busy: false };
      return { status: "error", label: "Failed", busy: false };
    default:
      return { status: "idle", label: "Disconnected", busy: false };
  }
}

/** `96 B`, `1.5 KiB`, `381 MiB` (IEC, like the TUI). */
export function formatSize(bytes: number): string {
  const units = ["KiB", "MiB", "GiB", "TiB"];
  if (bytes < 1024) return `${bytes} B`;
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

/** `42 s`, `3 min`, `1 h 20`. */
export function formatEta(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) return `${s} s`;
  if (s < 3600) return `${Math.floor(s / 60)} min`;
  return `${Math.floor(s / 3600)} h ${String(Math.floor((s % 3600) / 60)).padStart(2, "0")}`;
}

/** `<1 ms`, `4.2 ms`, `38 ms`. */
export function formatRtt(ms: number): string {
  if (ms < 1) return "<1 ms";
  if (ms < 10) return `${ms.toFixed(1)} ms`;
  return `${Math.round(ms)} ms`;
}
