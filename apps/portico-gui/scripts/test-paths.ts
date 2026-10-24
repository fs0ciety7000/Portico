// Checks of the home-relative path display (src/lib/api.ts, `displayPath`), run by `pnpm check`.
// Node's built-in test runner, no dependency.
import assert from "node:assert/strict";
import { test } from "node:test";
import { normalizeHome, shortenHome } from "../src/lib/api.ts";

test("Unix homes shorten to ~/ and keep backslashes as name characters", () => {
  const home = normalizeHome("/home/me/");
  assert.equal(home, "/home/me");
  assert.equal(shortenHome("/home/me/.ssh/config", home), "~/.ssh/config");
  assert.equal(shortenHome("/home/me", home), "~");
  assert.equal(shortenHome("/home/me/odd\\name", home), "~/odd\\name");
  assert.equal(shortenHome("/home/meme/x", home), "/home/meme/x");
  assert.equal(shortenHome("/home/me\\x", home), "/home/me\\x");
  assert.equal(shortenHome("/etc/ssh/ssh_config", home), "/etc/ssh/ssh_config");
});

test("Windows homes accept either separator and show the rest with /", () => {
  const home = normalizeHome("C:\\Users\\me\\");
  assert.equal(home, "C:\\Users\\me");
  assert.equal(shortenHome("C:\\Users\\me\\.ssh\\id_ed25519", home), "~/.ssh/id_ed25519");
  assert.equal(shortenHome("C:\\Users\\me/.ssh/config", home), "~/.ssh/config");
  assert.equal(shortenHome("C:\\Users\\me", home), "~");
  assert.equal(shortenHome("C:\\Users\\meme\\x", home), "C:\\Users\\meme\\x");
  assert.equal(shortenHome("D:\\Downloads\\x", home), "D:\\Downloads\\x");
});

test("no home, or an empty one, shortens nothing", () => {
  assert.equal(normalizeHome(null), null);
  assert.equal(normalizeHome(""), null);
  assert.equal(normalizeHome("/"), null);
  assert.equal(shortenHome("/home/me/x", null), "/home/me/x");
});
