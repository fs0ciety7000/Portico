// Checks of the host list layout (src/lib/hostlist.ts), run by `pnpm check`.
// Node's built-in test runner, no dependency.
import assert from "node:assert/strict";
import { test } from "node:test";
import type { Host, RecentHost } from "../src/lib/api.ts";
import { buildRows, relocate, rowHost } from "../src/lib/hostlist.ts";

const host = (alias: string, group: string | null = null): Host => ({
  alias,
  hostname: "127.0.0.1",
  user: null,
  port: 22,
  proxyJump: [],
  identityFiles: [],
  localForwards: [],
  remoteForwards: [],
  dynamicForwards: [],
  tags: [],
  group,
  favorite: false,
  notes: null,
  source: "~/.ssh/config:1",
  origin: "ssh_config",
  warnings: [],
});
const recent = (...aliases: string[]): RecentHost[] => aliases.map((alias, i) => ({ alias, last: 100 - i, count: 1 }));
const rows = (hosts: Host[], r: RecentHost[] = []) => buildRows(hosts, r, {}, false);

test("an edit that moves a host to a group keeps it selected", () => {
  const before = rows([host("alpha"), host("jumped"), host("gamma")], recent("alpha"));
  const at = before.findIndex((r) => r.key === "other:jumped");
  const after = rows([host("alpha"), host("jumped", "Lab"), host("gamma")], recent("alpha"));
  const moved = relocate(after, before[at]);
  assert.equal(rowHost(after[moved])?.alias, "jumped");
  assert.equal(after[moved]?.section.id, "group:Lab");
  // The index alone would land on another row (here the "Other" header).
  assert.notEqual(after[at]?.key, after[moved]?.key);
});

test("the first connection (\"Recent\" appears) keeps the host selected", () => {
  const before = rows([host("alpha"), host("beta")]);
  const at = before.findIndex((r) => r.key === "flat:beta");
  const after = rows([host("alpha"), host("beta")], recent("beta"));
  const moved = relocate(after, before[at]);
  assert.equal(after[moved]?.key, "other:beta");
});

test("a host removed from Recent leaves the index (the next recent host)", () => {
  const before = rows([host("a"), host("b"), host("c")], recent("a", "b"));
  const at = before.findIndex((r) => r.key === "recent:a");
  const after = rows([host("a"), host("b"), host("c")], recent("b"));
  assert.equal(relocate(after, before[at]), -1);
  assert.equal(after[at]?.key, "recent:b");
});

test("a header or a vanished host has nowhere to go", () => {
  const before = rows([host("a", "g"), host("b")]);
  assert.equal(relocate(rows([host("b")]), before.find((r) => r.key === "header:group:g")), -1);
  assert.equal(relocate(rows([host("b")]), before.find((r) => r.key === "group:g:a")), -1);
  assert.equal(relocate(before, undefined), -1);
});
