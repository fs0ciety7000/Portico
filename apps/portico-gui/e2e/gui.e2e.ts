// Desktop E2E: the real app (Tauri + WebKitGTK, not the browser demo)
// against a throwaway OpenSSH sshd. Started by e2e/run.sh, which sets up
// the environment (e2e/env.sh) and the WebDriver server; see docs/TESTING.md.
//
// One scenario, in order (each step needs the previous ones): startup and
// host key, terminal, vault, key generation, ssh-agent, copy-id and its
// check, login with the new key only, snippets, host metadata, tunnel,
// restart with the vault unlocked. Screenshots go to $E2E_ROOT/shots.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { Key, Session } from "./webdriver.ts";

const env = (name: string): string => {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set: run e2e/run.sh`);
  return value;
};
const root = env("E2E_ROOT");
const home = env("HOME");
const remoteHome = env("E2E_REMOTE_HOME");
const configDir = join(env("XDG_CONFIG_HOME"), "portico");
const dataDir = join(env("XDG_DATA_HOME"), "portico");
const shots = join(root, "shots");

const VAULT_PASS = "vault-pass-123";
const KEY_PASS = "key-pass-456";
const KEY = "portico_e2e";

let s: Session;
let shot = 0;

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Polls `check` until it returns something truthy. */
async function waitFor<T>(what: string, check: () => Promise<T> | T, timeout = 15_000): Promise<NonNullable<T>> {
  const end = Date.now() + timeout;
  let last: unknown;
  for (;;) {
    try {
      const value = await check();
      if (value) return value as NonNullable<T>;
    } catch (e) {
      last = e;
    }
    if (Date.now() > end) {
      await capture(`timeout-${what.replace(/\W+/g, "-")}`).catch(() => {});
      throw new Error(`timed out waiting for ${what}${last ? `: ${String(last)}` : ""}`);
    }
    await sleep(150);
  }
}

async function capture(name: string): Promise<void> {
  mkdirSync(shots, { recursive: true });
  shot++;
  writeFileSync(join(shots, `${String(shot).padStart(2, "0")}-${name}.png`), await s.screenshot());
}

/** innerText of the first match ("" when absent). */
const text = (css: string) => s.exec<string>("return document.querySelector(arguments[0])?.innerText ?? ''", css);
const modal = () => text(".modal");
const status = () => text(".statusbar .session");
const title = () => text(".statusbar .session .title");

/** Waits for a dialog showing `needle`, then for it to arm (Modal.svelte: 400 ms without keys). */
async function dialog(needle: string): Promise<void> {
  // innerText follows `text-transform`: compare without case.
  await waitFor(`dialog "${needle}"`, async () => (await modal()).toLowerCase().includes(needle.toLowerCase()));
  await sleep(700);
}
const noDialog = () => waitFor("no dialog", async () => !(await s.find(".modal")));

const type = (t: string) => s.typeFocused(t);
/** The active tab's shell is open (keys typed before that are not sent). */
const shellReady = () =>
  waitFor("shell", () =>
    s.exec<boolean>(
      "return !!document.querySelector('section.terminal:not(.hidden)[aria-busy=false]') && !!document.activeElement?.classList.contains('xterm-helper-textarea')",
    ),
  );
const press = (...chord: string[]) => s.keys(...chord);
/** Clicks the button of the open dialog whose text starts with `label`. */
const clickButton = (label: string) =>
  s.exec(
    `const b = [...document.querySelectorAll('.modal button')].find((b) => b.innerText.trim().startsWith(arguments[0]));
     if (!b) throw new Error('no button ' + arguments[0]); b.click();`,
    label,
  );

async function palette(command: string): Promise<void> {
  await press(Key.Control, Key.Shift, "K");
  await sleep(300);
  await type(command);
  await sleep(200);
  await press(Key.Enter);
}

/** Back to the host list, nothing focused: plain keys are shortcuts there. */
async function hostsView(): Promise<void> {
  await press(Key.Alt, "0");
  await sleep(200);
  await s.exec("document.activeElement?.blur()");
}

async function selectHost(alias: string): Promise<void> {
  await hostsView();
  await s.exec(
    `const row = [...document.querySelectorAll('.hosts li[role=option]')].find((r) => r.querySelector('.name')?.innerText.trim().startsWith(arguments[0]));
     if (!row) throw new Error('no row ' + arguments[0]); row.click(); document.activeElement?.blur();`,
    alias,
  );
  await waitFor(`${alias} selected`, async () => (await text(".hosts li[aria-selected=true] .name")).trim().startsWith(alias));
}

const agentList = () => {
  try {
    return execFileSync("ssh-add", ["-l"], { encoding: "utf8" });
  } catch (e) {
    return String((e as { stdout?: string }).stdout ?? ""); // exit 1: no identities
  }
};
const fingerprint = (path: string) => execFileSync("ssh-keygen", ["-l", "-f", path], { encoding: "utf8" }).split(" ")[1] ?? "";

before(async () => {
  s = await Session.create(env("E2E_DRIVER"), env("E2E_APP"));
});

after(async () => {
  await s?.quit().catch(() => {});
});

test("startup: hosts from ~/.ssh/config, no vault yet", async () => {
  await waitFor("3 hosts", async () => (await text(".statusbar")).includes("3 hosts"));
  await capture("hosts");
  assert.ok(!existsSync(join(dataDir, "vault.toml")), "no vault before it is created");
});

test("connect: new host key, terminal command, status and latency", async () => {
  await selectHost("alpha");
  await press(Key.Enter);
  await dialog("New host key");
  await capture("host-key");
  await press("y");
  await waitFor("connected", async () => (await status()).includes("Connected"));
  assert.match(readFileSync(join(home, ".ssh/known_hosts"), "utf8"), /^\[127\.0\.0\.1\]:\d+ ssh-ed25519 /);
  // Output comes back through the PTY channel and xterm.js: the shell
  // computes a title (OSC 0) that the status bar shows.
  await shellReady();
  await type(`printf '\\033]0;e2e-%s\\007' $((6*7))`);
  await press(Key.Enter);
  await waitFor("title from the shell", async () => (await title()) === "e2e-42");
  assert.ok(await s.find(".statusbar .rtt"), "latency shown");
  await capture("terminal");
});

test("recording: confirm, REC from the engine event, stop, list, play", async () => {
  const recDir = join(dataDir, "recordings");
  // Ctrl-Shift-R in the terminal: the armed confirmation, with the folder.
  await press(Key.Control, Key.Shift, "R");
  await dialog("Record this terminal?");
  assert.match(await modal(), /What you type is never recorded/);
  assert.match(await modal(), /recordings\/, readable only by you/);
  await capture("record-confirm");
  await press(Key.Enter);
  await noDialog();
  await waitFor("REC in the status bar", async () => (await text(".statusbar")).includes("REC"));
  await shellReady();
  await type("echo rec-e2e-$((6*7))");
  await press(Key.Enter);
  await sleep(1500);
  await capture("recording");
  // Stopping needs no confirmation; REC goes with the `stopped` event.
  await press(Key.Control, Key.Shift, "R");
  await waitFor("REC gone", async () => !(await text(".statusbar")).includes("REC"));
  assert.equal(await s.find(".modal"), null, "no confirmation to stop");
  const files = readdirSync(recDir).filter((f) => f.endsWith(".cast"));
  assert.equal(files.length, 1, `one recording in ${recDir}`);
  const file = join(recDir, files[0] ?? "");
  const cast = readFileSync(file, "utf8");
  assert.match(cast.split("\n")[0] ?? "", /"version":2/);
  assert.ok(cast.includes("rec-e2e-42"), "the output is recorded");
  assert.equal(statSync(file).mode & 0o777, 0o600);
  // The list, then the player (read-only, its own xterm.js instance).
  await palette("Recordings");
  await dialog(files[0] ?? "?");
  assert.match(await modal(), /alpha/);
  await capture("recordings");
  await press(Key.Enter);
  await waitFor("player", () => s.find(".player .xterm-screen"));
  // The end: the play button offers "Replay".
  await waitFor("played to the end", () => s.find(".player button[aria-label=Replay]"), 20_000);
  // The player's own emulator (DOM renderer: its rows are readable) shows
  // the recorded output.
  await waitFor("recorded output in the player", async () => (await text(".player .xterm-rows")).includes("rec-e2e-42"));
  await sleep(300);
  await capture("player");
  await press(Key.Escape);
  await dialog("Recordings");
  await press(Key.Escape);
  await noDialog();
  await shellReady();
});

test("vault: created from the keys view", async () => {
  await palette("Keys");
  await dialog("Local keys");
  assert.match(await modal(), /No vault yet/);
  await type("V");
  await dialog("passphrase");
  await type(VAULT_PASS);
  await press(Key.Tab);
  await type(VAULT_PASS);
  await press(Key.Enter);
  await waitFor("vault unlocked", async () => (await modal()).includes("Vault unlocked"));
  assert.ok(existsSync(join(dataDir, "vault.toml")));
  assert.ok(!readFileSync(join(dataDir, "vault.toml"), "utf8").includes(VAULT_PASS));
});

test("keys: ed25519 with a passphrase", async () => {
  await sleep(500);
  await type("n");
  await dialog("New key");
  await press(Key.Control, "a");
  await type(KEY);
  await press(Key.Tab);
  await press(Key.Tab);
  await type(KEY_PASS);
  await press(Key.Tab);
  await type(KEY_PASS);
  await capture("keygen");
  await press(Key.Enter);
  const priv = join(home, ".ssh", KEY);
  await waitFor("key files", () => existsSync(priv) && existsSync(`${priv}.pub`));
  // Encrypted: the passphrase is needed to read it back.
  assert.throws(() => execFileSync("ssh-keygen", ["-y", "-P", "", "-f", priv], { stdio: "pipe" }));
  execFileSync("ssh-keygen", ["-y", "-P", KEY_PASS, "-f", priv], { stdio: "pipe" });
  await waitFor("key listed", async () => (await modal()).includes(KEY));
});

test("ssh-agent: add for an hour, then remove", async () => {
  const fp = fingerprint(join(home, ".ssh", `${KEY}.pub`));
  await dialog(KEY);
  await type("a");
  await dialog("Add to ssh-agent");
  await clickButton("1 hour");
  await press(Key.Enter);
  await dialog("This key is encrypted");
  await type(KEY_PASS);
  await press(Key.Enter);
  await waitFor("in the agent", () => agentList().includes(fp));
  await waitFor("agent row", async () => (await modal()).includes("Remove from agent"));
  await capture("agent");
  await sleep(500);
  await type("x");
  await dialog("Remove from ssh-agent?");
  await type("y");
  await waitFor("agent empty", () => !agentList().includes(fp));
});

test("copy-id: install, verify with the key (passphrase prompt), remember it", async () => {
  const pub = readFileSync(join(home, ".ssh", `${KEY}.pub`), "utf8").trim();
  await dialog("Copy to host");
  await type("c");
  await dialog("then logs in with it to check");
  await type("alpha");
  await press(Key.Enter);
  // The check logs in with the new key: its passphrase is asked for.
  await dialog("Key passphrase");
  assert.match(await modal(), /Verifying/);
  await type(KEY_PASS);
  await s.exec("document.querySelector('.modal .check input').click()");
  await capture("probe-passphrase");
  await press(Key.Enter);
  await waitFor("verified", async () => (await modal()).includes("Installed and verified"));
  await capture("copy-id");
  assert.ok(readFileSync(join(remoteHome, ".ssh/authorized_keys"), "utf8").includes(pub.split(" ")[1] ?? "?"));
  await press(Key.Enter); // Done: back to the keys view (a new dialog, armed anew)
  await dialog("Local keys");
  await press(Key.Escape);
  await noDialog();
});

test("login with the new key only (passphrase from the vault)", async () => {
  await selectHost("gamma");
  await press(Key.Enter);
  await waitFor("gamma connected", async () => {
    const st = await status();
    return st.includes("gamma") && st.includes("Connected");
  });
  assert.equal(await s.find(".modal"), null, "no passphrase prompt");
  await shellReady();
});

test("files: SFTP download, deep path kept in view", async () => {
  await press(Key.Control, Key.Shift, "F");
  await waitFor("remote listing", async () => (await text(".list[aria-label='Remote files']")).includes("hello.txt"));
  // The throwaway remote HOME is deep: the current folder stays visible.
  const crumbVisible = await s.exec<boolean>(`
    const nav = document.querySelector("nav.crumbs"), cur = nav?.querySelector("button.current");
    if (!nav || !cur) return false;
    const a = nav.getBoundingClientRect(), b = cur.getBoundingClientRect();
    return cur.innerText === "remote" && b.right <= a.right + 1 && b.left >= a.left - 1;`);
  assert.ok(crumbVisible, "current folder visible in the path bar");
  await s.exec(
    `const row = [...document.querySelectorAll(".list [role=option]")].find((r) => r.innerText.includes("hello.txt"));
     row.click(); document.querySelector(".list").focus();`,
  );
  await sleep(200);
  await press(Key.Enter);
  const target = join(home, "Downloads", "hello.txt");
  await waitFor("downloaded", () => existsSync(target) && readFileSync(target, "utf8") === "hello from the server\n");
  await capture("files");
  await press(Key.Escape); // back to the terminal
  await shellReady();
});

test("snippets: placeholder run, paste mode refused under sh", async () => {
  await press(Key.Control, Key.Shift, "S");
  await dialog("Snippets");
  await type("n");
  await dialog("New snippet");
  await type("Greet");
  await press(Key.Tab);
  await type(`printf '\\033]0;snip-%s\\007' {{who}}`);
  await sleep(300);
  await capture("snippet-new");
  await press(Key.Control, "s");
  await waitFor("saved", async () => (await modal()).includes("Greet"));
  assert.match(readFileSync(join(configDir, "snippets.toml"), "utf8"), /\{\{who\}\}/);
  await sleep(500);
  await press(Key.Enter);
  await dialog("Run Greet");
  await type("world");
  await press(Key.Enter);
  await waitFor("snippet ran", async () => (await title()) === "snip-world");
  await capture("snippet");

  // Without bracketed paste (dash), a multi-line paste is refused.
  await shellReady();
  await type("exec sh");
  await press(Key.Enter);
  await sleep(500);
  await press(Key.Control, Key.Shift, "S");
  await dialog("Snippets");
  await type("n");
  await dialog("New snippet");
  await type("Two lines");
  await press(Key.Tab);
  await type("echo one");
  await press(Key.Enter);
  await type("echo two");
  await s.exec("document.querySelector('.modal .check input').click()"); // paste only
  await press(Key.Control, "s");
  await waitFor("saved", async () => (await modal()).includes("Two lines"));
  await sleep(500);
  await press(Key.Down);
  await press(Key.Enter);
  await waitFor("refused", async () => (await modal()).includes("cannot be pasted without bracketed paste"));
  await capture("paste-refused");
  await sleep(500);
  await type("r"); // run it instead
  await noDialog();
});

test("host metadata: favorite, tags, group, notes in hosts.toml; selection follows", async () => {
  await selectHost("jumped");
  await press(Key.Control, Key.Shift, "D");
  await waitFor("favorite", () => existsSync(join(configDir, "hosts.toml")) && readFileSync(join(configDir, "hosts.toml"), "utf8").includes("favorite = true"));
  await type("e");
  await dialog("Edit jumped");
  await type("bastion e2e ");
  await press(Key.Tab);
  await type("Lab");
  await press(Key.Tab);
  await type("Reached through alpha.");
  await press(Key.Control, "s");
  await noDialog();
  const toml = readFileSync(join(configDir, "hosts.toml"), "utf8");
  assert.match(toml, /\[hosts\.jumped\]/);
  assert.match(toml, /tags = \["bastion", "e2e"\]/);
  assert.match(toml, /group = "Lab"/);
  assert.match(toml, /notes = "Reached through alpha\."/);
  // The host moved to the "Lab" section and is still the selected row.
  await waitFor("selection kept", async () => (await text(".hosts li[aria-selected=true] .name")).trim().startsWith("jumped"));
  await capture("edited");
});

test("ProxyJump, then the logs panel with the effective config", async () => {
  await press(Key.Enter);
  await waitFor("jumped connected", async () => {
    const st = await status();
    return st.includes("jumped") && st.includes("Connected");
  });
  await press(Key.Control, Key.Shift, "L");
  await dialog("Session logs");
  await waitFor("jump record", async () => (await modal()).includes("opening direct-tcpip"));
  await capture("logs");
  await s.exec("document.activeElement?.blur()");
  await type("jjj"); // the first alpha session
  await press(Key.Tab);
  await waitFor("effective config", async () => (await modal()).includes("PORTICO_E2E_TOKEN=•••"));
  assert.equal(await s.exec<boolean>("return document.documentElement.outerHTML.includes('s3cr3t')"), false);
  await capture("effective-config");
  await press(Key.Escape);
  await noDialog();
});

test("tunnel -L: shown with its spec, carries traffic, stops", async () => {
  const port = await freePort();
  const web = env("E2E_WEB_PORT");
  await palette("Tunnels");
  await dialog("Tunnels");
  await type(`${port}:127.0.0.1:${web}`);
  await press(Key.Enter);
  await waitFor("listening", async () => (await modal()).includes(`127.0.0.1:${port}`));
  assert.match(await text(".modal li .spec"), new RegExp(`-L .*${port}.*→ 127\\.0\\.0\\.1:${web}`));
  const body = await (await fetch(`http://127.0.0.1:${port}/index.html`)).text();
  assert.equal(body.trim(), "portico-e2e-tunnel");
  await capture("tunnel");
  await clickButton("Stop");
  await waitFor("stopped", async () => (await modal()).includes("No tunnels yet"));
  await press(Key.Escape);
  await noDialog(); // keys pressed during its closing transition still go to the dialog
});

test("restart: theme kept, vault unlocked at startup, key login without prompt", async () => {
  await hostsView();
  await type("t");
  await waitFor("theme saved", () => existsSync(join(configDir, "settings.toml")) && readFileSync(join(configDir, "settings.toml"), "utf8").includes("basalt"));
  await s.quit();
  s = await Session.create(env("E2E_DRIVER"), env("E2E_APP"));
  await dialog("Unlock the vault");
  assert.equal((await text(".theme-name")).trim(), "Basalt");
  await type(VAULT_PASS);
  await press(Key.Enter);
  await noDialog();
  await selectHost("gamma");
  await press(Key.Enter);
  await waitFor("gamma connected", async () => (await status()).includes("Connected"));
  assert.equal(await s.find(".modal"), null, "no passphrase prompt");
  await capture("restart");
});

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const srv = createServer();
    srv.listen(0, "127.0.0.1", () => {
      const address = srv.address();
      srv.close(() => (typeof address === "object" && address ? resolve(address.port) : reject(new Error("no port"))));
    });
  });
}
