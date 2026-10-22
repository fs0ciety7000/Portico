// Checks of the host form logic (src/lib/hostform.ts), run by `pnpm check`.
// Node's built-in test runner, no dependency.
import assert from "node:assert/strict";
import { test } from "node:test";
import type { Host, LocalKey } from "../src/lib/api.ts";
import { capitalizable, emptyForm, fieldOf, FIELD_ORDER, formFromHost, isDirty, KEEP_IDENTITY, parsePort, suggestAlias, toDraft } from "../src/lib/hostform.ts";

const key = (id: string, hasPrivate = true): LocalKey => ({
  id,
  privateDisplay: `~/.ssh/${id}`,
  publicDisplay: `~/.ssh/${id}.pub`,
  algorithm: "ED25519",
  fingerprint: "SHA256:x",
  comment: "",
  hasPrivate,
  certificate: null,
});

const portico = (identityFile: string | null): Host => ({
  alias: "nas",
  hostname: "nas.lan",
  // Effective values may be inherited (`Host *`): the form ignores them.
  user: "inherited",
  port: 22,
  proxyJump: [],
  identityFiles: identityFile ? [identityFile, "~/.ssh/other"] : [],
  localForwards: [],
  remoteForwards: [],
  dynamicForwards: [],
  tags: ["home"],
  group: "Lab",
  favorite: true,
  notes: null,
  source: "~/.config/portico/hosts.conf:5",
  origin: "portico",
  managed: { hostname: "nas.lan", user: null, port: 2222, identityFile, proxyJump: "bastion" },
  warnings: [],
});

test("engine error codes map to their field, by code only", () => {
  assert.equal(fieldOf("host_invalid_alias"), "alias");
  assert.equal(fieldOf("host_alias_taken"), "alias");
  assert.equal(fieldOf("host_invalid_hostname"), "hostname");
  assert.equal(fieldOf("host_invalid_user"), "user");
  assert.equal(fieldOf("host_invalid_port"), "port");
  assert.equal(fieldOf("host_invalid_identity_file"), "identity");
  assert.equal(fieldOf("key_unknown"), "identity");
  assert.equal(fieldOf("key_public_only"), "identity");
  assert.equal(fieldOf("host_invalid_proxy_jump"), "proxyJump");
  assert.equal(fieldOf("host_proxy_jump_cycle"), "proxyJump");
  assert.equal(fieldOf("host_tag"), "tags");
  assert.equal(fieldOf("host_group"), "group");
  assert.equal(fieldOf("host_notes"), "notes");
  assert.equal(fieldOf("vault_locked"), "password");
  // Not a field's: shown under the form.
  for (const code of ["host_file_unmanaged", "host_file_invalid", "host_in_use", "host_read_only", "host_not_found", "io", "toString", "__proto__"]) {
    assert.equal(fieldOf(code), null, code);
  }
  // Messages that start with an alias are never capitalized.
  assert.equal(capitalizable("host_alias_taken"), false);
  assert.equal(capitalizable("host_in_use"), false);
  assert.equal(capitalizable("host_invalid_port"), true);
  // Every field a code maps to is in the focus order.
  for (const code of ["host_invalid_alias", "host_invalid_port", "host_notes", "secrets"]) assert.ok(FIELD_ORDER.includes(fieldOf(code)!));
});

test("a Portico host's form starts from its own directives", () => {
  const form = formFromHost(portico("~/.ssh/id_ed25519"), [key("id_rsa"), key("id_ed25519")]);
  assert.equal(form.user, "", "the inherited user is not the host's");
  assert.equal(form.port, "2222");
  assert.equal(form.identity, "key:id_ed25519");
  assert.equal(form.proxyJump, "bastion");
  assert.deepEqual(form.tags, ["home"]);
  assert.equal(form.group, "Lab");
  assert.equal(form.passwordMode, "keep");
  // A key that is not discovered (or public only) is kept, never sent as a path.
  assert.equal(formFromHost(portico("~/keys/old"), [key("id_ed25519")]).identity, KEEP_IDENTITY);
  assert.equal(formFromHost(portico("~/.ssh/id_ed25519"), [key("id_ed25519", false)]).identity, KEEP_IDENTITY);
  assert.equal(formFromHost(portico(null), [key("id_ed25519")]).identity, "");
});

test("dirty tracking: edits, reverted edits, password choices", () => {
  const initial = formFromHost(portico("~/.ssh/id_ed25519"), [key("id_ed25519")]);
  const form = { ...initial, tags: [...initial.tags] };
  assert.equal(isDirty(initial, form), false);
  form.hostname = "nas.lan "; // spaces only
  assert.equal(isDirty(initial, form), false);
  form.port = "22";
  assert.equal(isDirty(initial, form), true);
  form.port = "2222";
  assert.equal(isDirty(initial, form), false);
  form.tags.push("nas");
  assert.equal(isDirty(initial, form), true);
  form.tags.pop();
  form.passwordMode = "forget";
  assert.equal(isDirty(initial, form), true);
  form.passwordMode = "keep";
  form.password = "x";
  assert.equal(isDirty(initial, form), true);
  assert.equal(isDirty(emptyForm(), emptyForm()), false);
});

test("drafts: port parsing, key choice by id, password choices", () => {
  assert.equal(parsePort(""), null);
  assert.equal(parsePort(" 22 "), 22);
  assert.equal(parsePort("70000"), 70000, "out of range: the engine says so");
  assert.equal(parsePort("22a"), undefined);
  assert.equal(parsePort("-1"), undefined);
  assert.equal(parsePort("99999999999"), undefined);

  const form = { ...emptyForm(), alias: " web ", hostname: "10.0.0.1", user: " ", port: "x" };
  const bad = toDraft(form, false);
  assert.ok(!bad.ok && bad.field === "port" && bad.error.code === "host_invalid_port");

  form.port = "2200";
  form.identity = "key:id_ed25519";
  form.password = "s3cret";
  const created = toDraft(form, false);
  assert.ok(created.ok);
  assert.equal(created.draft.alias, "web");
  assert.equal(created.draft.user, null);
  assert.equal(created.draft.port, 2200);
  assert.deepEqual(created.draft.identity, { kind: "key", id: "id_ed25519" });
  assert.deepEqual(created.draft.password, { kind: "set", value: "s3cret" });
  assert.equal(created.draft.proxyJump, null);
  // No field of the draft names a path.
  assert.ok(!JSON.stringify(created.draft).includes("/"));

  form.password = "";
  const noPassword = toDraft(form, false);
  assert.ok(noPassword.ok && noPassword.draft.password.kind === "keep");

  form.identity = KEEP_IDENTITY;
  const kept = toDraft(form, true);
  assert.ok(kept.ok && kept.draft.identity.kind === "keep");
  const createdKeep = toDraft(form, false);
  assert.ok(createdKeep.ok && createdKeep.draft.identity.kind === "none");

  form.passwordMode = "set";
  const empty = toDraft(form, true);
  assert.ok(!empty.ok && empty.field === "password");
  form.passwordMode = "forget";
  form.password = "ignored";
  const forgot = toDraft(form, true);
  assert.ok(forgot.ok && forgot.draft.password.kind === "forget");
});

test("duplicate alias suggestion is a valid, free alias", () => {
  assert.equal(suggestAlias("web-01", []), "web-01-copy");
  assert.equal(suggestAlias("Web Prod", ["web-prod-copy"]), "web-prod-copy-2");
  assert.equal(suggestAlias(".hidden", []), "hidden-copy");
  assert.match(suggestAlias("x".repeat(80), []), /^x{55}-copy$/);
});
