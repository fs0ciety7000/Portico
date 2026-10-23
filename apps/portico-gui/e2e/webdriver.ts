// Minimal W3C WebDriver client (fetch only, no dependency) for driving the
// real desktop app through `tauri-driver` + `WebKitWebDriver`.
// See docs/TESTING.md.

const ELEMENT = "element-6066-11e4-a52e-4f735466cecf";

export type El = { id: string };

/** Keys of the WebDriver "special keys" table. */
export const Key = {
  Enter: "",
  Escape: "",
  Tab: "",
  Backspace: "",
  Control: "",
  Shift: "",
  Alt: "",
  Down: "",
  Up: "",
  Delete: "",
} as const;

export class Session {
  readonly base: string;
  readonly id: string;

  constructor(base: string, id: string) {
    this.base = base;
    this.id = id;
  }

  static async create(base: string, application: string): Promise<Session> {
    const value = await call(base, "POST", "/session", {
      capabilities: { alwaysMatch: { browserName: "wry", "tauri:options": { application } } },
    });
    return new Session(base, (value as { sessionId: string }).sessionId);
  }

  private req(method: string, path: string, body?: unknown): Promise<unknown> {
    return call(this.base, method, `/session/${this.id}${path}`, body);
  }

  async quit(): Promise<void> {
    await this.req("DELETE", "");
  }

  async find(css: string): Promise<El | null> {
    const all = await this.findAll(css);
    return all[0] ?? null;
  }

  async findAll(css: string): Promise<El[]> {
    const value = (await this.req("POST", "/elements", { using: "css selector", value: css })) as Record<string, string>[];
    return value.map((v) => ({ id: String(v[ELEMENT]) }));
  }

  async click(el: El): Promise<void> {
    await this.req("POST", `/element/${el.id}/click`, {});
  }

  async type(el: El, text: string): Promise<void> {
    await this.req("POST", `/element/${el.id}/value`, { text });
  }

  async text(el: El): Promise<string> {
    return String(await this.req("GET", `/element/${el.id}/text`));
  }

  /** Runs `script` (a function body) in the page; `arguments` holds `args`. */
  async exec<T = unknown>(script: string, ...args: unknown[]): Promise<T> {
    return (await this.req("POST", "/execute/sync", { script, args })) as T;
  }

  /** A key chord sent to whatever has the focus (modifiers stay down). */
  async keys(...chord: string[]): Promise<void> {
    const down = chord.map((value) => ({ type: "keyDown", value }));
    const up = [...chord].reverse().map((value) => ({ type: "keyUp", value }));
    await this.req("POST", "/actions", { actions: [{ type: "key", id: "kbd", actions: [...down, ...up] }] });
    await this.req("DELETE", "/actions");
  }

  /** Types a string into the focused element, one key at a time. */
  async typeFocused(text: string): Promise<void> {
    const actions = [...text].flatMap((value) => [
      { type: "keyDown", value },
      { type: "keyUp", value },
    ]);
    await this.req("POST", "/actions", { actions: [{ type: "key", id: "kbd", actions }] });
  }

  /** Moves the mouse to viewport point (x, y), then clicks there with `modifier` held. */
  async modifierClick(x: number, y: number, modifier: string): Promise<void> {
    const at = { type: "pointerMove", x: Math.round(x), y: Math.round(y), origin: "viewport", duration: 0 };
    const mouse = { type: "pointer", id: "mouse", parameters: { pointerType: "mouse" } };
    // Hover first: xterm.js finds the link under the pointer on mouse moves.
    await this.req("POST", "/actions", { actions: [{ ...mouse, actions: [at, { ...at, x: at.x + 1 }] }] });
    await new Promise((r) => setTimeout(r, 300));
    await this.req("POST", "/actions", {
      actions: [
        { type: "key", id: "kbd", actions: [{ type: "keyDown", value: modifier }, { type: "pause" }, { type: "pause" }, { type: "pause" }, { type: "keyUp", value: modifier }] },
        { ...mouse, actions: [{ type: "pause" }, at, { type: "pointerDown", button: 0 }, { type: "pointerUp", button: 0 }, { type: "pause" }] },
      ],
    });
    await this.req("DELETE", "/actions");
  }

  async screenshot(): Promise<Buffer> {
    return Buffer.from(String(await this.req("GET", "/screenshot")), "base64");
  }
}

async function call(base: string, method: string, path: string, body?: unknown): Promise<unknown> {
  const res = await fetch(base + path, {
    method,
    headers: { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const json = (await res.json()) as { value: unknown };
  if (!res.ok) {
    const v = json.value as { error?: string; message?: string };
    throw new Error(`webdriver ${method} ${path}: ${v.error ?? res.status}: ${v.message ?? ""}`);
  }
  return json.value;
}
