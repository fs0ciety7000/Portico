// Fallback vault (used when no system keyring is available). The webview
// only holds the status; passphrases go straight to Rust (SecretString) and
// the dialogs clear their fields once sent.
import { asError, type ErrorSummary, type VaultStatus } from "../api";
import { backend } from "../backend";

export type VaultMode = "unlock" | "create" | "change";

/** Why a vault dialog is open (changes its wording). `host`: a host form
 * could not save its password (no vault, or a locked one); it saves again
 * once the vault is ready (B1). */
export type VaultReason = "startup" | "keys" | "remember" | "host";

class Vault {
  status: VaultStatus | null = $state(null);
  /** Open vault dialog, if any. */
  dialog: { mode: VaultMode; reason: VaultReason } | null = $state(null);
  /** The user chose "Continue without" at startup. */
  skipped = $state(false);
  /** Told how the open dialog ended: `true` once created or unlocked. */
  private then: ((ready: boolean) => void) | null = null;

  /** False when secrets are kept in the system keyring. */
  inUse: boolean = $derived(this.status !== null && this.status !== "not_in_use");

  async refresh(): Promise<VaultStatus | null> {
    try {
      this.status = await backend.vaultStatus();
    } catch {
      this.status = null;
    }
    return this.status;
  }

  /** At startup: a locked vault asks for its passphrase once. */
  async init() {
    if ((await this.refresh()) === "locked") this.dialog = { mode: "unlock", reason: "startup" };
  }

  open(mode: VaultMode, reason: VaultReason, then: ((ready: boolean) => void) | null = null) {
    this.then = then;
    this.dialog = { mode, reason };
  }

  /** Dismissed ("Not now", "Cancel", `Esc`). */
  close() {
    if (this.dialog?.reason === "startup" && this.status === "locked") this.skipped = true;
    this.settle(false);
    this.dialog = null;
  }

  /** Created, unlocked or changed: the dialog closes. */
  done() {
    this.settle(true);
    this.dialog = null;
  }

  private settle(ready: boolean) {
    const then = this.then;
    this.then = null;
    then?.(ready);
  }


  /** After a prompt answered with "remember": a missing or locked vault
   * could not keep the secret, so offer to fix that. */
  afterRemember() {
    if (this.status === "absent") this.open("create", "remember");
    else if (this.status === "locked") this.open("unlock", "remember");
  }

  /** Returns null on success, else the error to show inline. */
  async submit(mode: VaultMode, passphrase: string, previous = ""): Promise<ErrorSummary | null> {
    try {
      if (mode === "unlock") await backend.vaultUnlock(passphrase);
      else if (mode === "create") await backend.vaultCreate(passphrase);
      else await backend.vaultChangePassphrase(previous, passphrase);
      await this.refresh();
      this.skipped = false;
      return null;
    } catch (e) {
      await this.refresh();
      return asError(e);
    }
  }

  async lock(): Promise<ErrorSummary | null> {
    try {
      await backend.vaultLock();
      await this.refresh();
      return null;
    } catch (e) {
      return asError(e);
    }
  }
}

export const vault = new Vault();
