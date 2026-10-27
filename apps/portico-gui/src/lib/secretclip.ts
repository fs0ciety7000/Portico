// A secret copied to the clipboard (a generated sync passphrase, phase 11)
// is cleared after CLIPBOARD_CLEAR_MS if the clipboard still holds it.
//
// - Only a SHA-256 digest of the secret is kept for the comparison, never
//   the text itself.
// - The clipboard is read back to compare (`readText`, the same permission
//   the terminal's paste uses). If reading is refused, it is emptied anyway,
//   but only while this copy is still the latest one made through here: a
//   newer copy has its own timer. The choice (S1c security review, A4):
//   emptying a clipboard the user refilled meanwhile is a lesser harm than
//   leaving a passphrase in it.
// - The timer outlives the dialog that copied: closing it early does not
//   leave the secret behind.
//
// Pure module (no DOM types beyond the clipboard shape): tested under Node
// by scripts/test-sync.ts with a fake clipboard and a fake timer.

export const CLIPBOARD_CLEAR_MS = 30_000;

/** What the note under a copied secret says. */
export const COPIED_NOTE = "Copied — cleared from the clipboard in 30 s";

export interface ClipboardLike {
  writeText(text: string): Promise<void>;
  readText?(): Promise<string>;
}

type Schedule = (run: () => void, ms: number) => unknown;

let latest = 0;

async function digest(text: string): Promise<string | null> {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) return null;
  const bytes = new Uint8Array(await subtle.digest("SHA-256", new TextEncoder().encode(text)));
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

/** Copies `secret`, then clears it after 30 s if it is still there. The
 * returned promise settles once the clear has run (tests). */
export async function copySecret(
  secret: string,
  clipboard: ClipboardLike = navigator.clipboard,
  schedule: Schedule = setTimeout,
): Promise<{ cleared: Promise<boolean> }> {
  await clipboard.writeText(secret);
  const mine = ++latest;
  const fingerprint = await digest(secret);
  const cleared = new Promise<boolean>((resolve) => {
    schedule(() => void clearIfStillThere(mine, fingerprint, clipboard).then(resolve), CLIPBOARD_CLEAR_MS);
  });
  return { cleared };
}

async function clearIfStillThere(mine: number, fingerprint: string | null, clipboard: ClipboardLike): Promise<boolean> {
  try {
    if (!clipboard.readText || fingerprint === null) throw new Error("cannot compare");
    const now = await clipboard.readText();
    if ((await digest(now)) !== fingerprint) return false;
  } catch {
    // Cannot read it back: empty it only while this copy is the latest one.
    if (mine !== latest) return false;
  }
  try {
    await clipboard.writeText("");
    return true;
  } catch {
    return false;
  }
}
