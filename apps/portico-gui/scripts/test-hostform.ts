// Checks of the host form logic (src/lib/hostform.ts), run by `pnpm check`.
// Node's built-in test runner, no dependency.
import assert from "node:assert/strict";
import { test } from "node:test";
import type { Host, ManagedHost } from "../src/lib/api.ts";
import {
  accountText,
  cannotDeleteTitle,
  capitalizable,
  deleteConfirmationText,
  DESTINATION_CHANGED_NOTE,
  destinationChanged,
  emptyForm,
  fieldOf,
  FIELD_ORDER,
  formFromHost,
  IMPORTED_EDIT_HELP,
  INCLUDE_MESSAGE,
  isDirty,
  KEEP_IDENTITY,
  KEY_NONE_LABEL,
  NO_PASSWORD_REMEMBERED,
  originLabel,
  parsePort,
  PASSWORD_DESTINATION_NOTE,
  PASSWORD_REMEMBERED,
  READ_ONLY_IMPORTED,
  sendsPassword,
  suggestAlias,
  toDraft,
  vaultFix,
} from "../src/lib/hostform.ts";
import { sendsRememberedPassword } from "../src/lib/api.ts";

/** What hosts.conf holds (`managed_host`): the key mapped to a discovered id in Rust. */
const own = (identityFile: string | null, keyId: string | null, passwordRemembered = true): ManagedHost => ({
  alias: "nas",
  hostname: "nas.lan",
  user: null,
  port: 2222,
  proxyJump: "bastion",
  keyId,
  identityFile,
  passwordRemembered,
  importedFrom: null,
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
  importedFrom: null,
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

test("a Portico host's form starts from what hosts.conf holds", () => {
  const form = formFromHost(portico("~/.ssh/id_ed25519"), own("~/.ssh/id_ed25519", "id_ed25519"));
  assert.equal(form.user, "", "the effective user (inherited, or an earlier value elsewhere) is not the host's");
  assert.equal(form.hostname, "nas.lan");
  assert.equal(form.port, "2222");
  assert.equal(form.identity, "key:id_ed25519");
  assert.equal(form.proxyJump, "bastion");
  assert.deepEqual(form.tags, ["home"]);
  assert.equal(form.group, "Lab");
  assert.equal(form.passwordMode, "keep");
  // hosts.conf wins over the resolved values of the inventory.
  const hidden = formFromHost(portico(null), { ...own(null, null), user: "deploy", hostname: "10.0.0.1" });
  assert.equal(hidden.user, "deploy");
  assert.equal(hidden.hostname, "10.0.0.1");
  // A key that is not a discovered one is kept, never sent as a path.
  assert.equal(formFromHost(portico("~/keys/old"), own("~/keys/old", null)).identity, KEEP_IDENTITY);
  assert.equal(formFromHost(portico(null), own(null, null)).identity, "");
});

test("dirty tracking: edits, reverted edits, password choices", () => {
  const initial = formFromHost(portico("~/.ssh/id_ed25519"), own("~/.ssh/id_ed25519", "id_ed25519"));
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

test("the password destination note shows when a password is set or kept", () => {
  assert.equal(
    PASSWORD_DESTINATION_NOTE,
    "Sent automatically to the server that answers at this address, after its host key is verified; then tied to that key.",
    "same wording as the TUI",
  );
  const form = emptyForm();
  assert.equal(sendsPassword(form, false), false, "creating, nothing typed");
  assert.equal(sendsPassword({ ...form, password: "s3cret" }, false), true, "creating, typed");
  assert.equal(sendsPassword({ ...form, passwordMode: "keep" }, true), true, "editing, kept");
  assert.equal(sendsPassword({ ...form, passwordMode: "set" }, true), false, "editing, nothing typed yet");
  assert.equal(sendsPassword({ ...form, passwordMode: "set", password: "x" }, true), true, "editing, new one typed");
  assert.equal(sendsPassword({ ...form, passwordMode: "forget" }, true), false, "editing, forgotten");
});

test("B1: a password with no vault (or a locked one) chains the vault dialog", () => {
  assert.equal(vaultFix("vault_missing"), "create");
  assert.equal(vaultFix("vault_locked"), "unlock");
  for (const code of ["vault_corrupt", "vault_wrong_passphrase", "secrets", "host_invalid_port"]) assert.equal(vaultFix(code), null, code);
  // Both still belong to the password field (shown there after "Not now").
  assert.equal(fieldOf("vault_missing"), "password");
  assert.equal(fieldOf("vault_locked"), "password");
  // The draft keeps the typed password: retried as it is once the vault is ready.
  const form = { ...emptyForm(), alias: "nas", hostname: "nas.lan", password: "hunter2" };
  const draft = toDraft(form, false);
  assert.ok(draft.ok);
  assert.deepEqual(draft.ok && draft.draft.password, { kind: "set", value: "hunter2" });
});

test("B3: remembered or not, and a changed destination", () => {
  // Nothing remembered: the form starts on Change, empty, which changes nothing.
  const none = formFromHost(portico(null), own(null, null, false));
  assert.equal(none.passwordMode, "set");
  const draft = toDraft(none, true, false);
  assert.ok(draft.ok);
  assert.deepEqual(draft.ok && draft.draft.password, { kind: "keep" });
  assert.equal(isDirty(none, none), false);
  assert.equal(sendsPassword(none, true, false), false);
  assert.equal(sendsPassword({ ...none, passwordMode: "keep" }, true, false), false, "Keep sends nothing when nothing is remembered");
  // Remembered: Keep, and an empty Change asks for the password.
  const kept = formFromHost(portico(null), own(null, null, true));
  assert.equal(kept.passwordMode, "keep");
  assert.equal(toDraft({ ...kept, passwordMode: "set" }, true, true).ok, false);
  // HostName, User or Port changed (trimmed): the Keep note is replaced.
  assert.equal(destinationChanged(kept, { ...kept, tags: ["x"], notes: "n" }), false);
  assert.equal(destinationChanged(kept, { ...kept, hostname: " nas.lan " }), false);
  assert.equal(destinationChanged(kept, { ...kept, hostname: "nas2.lan" }), true);
  assert.equal(destinationChanged(kept, { ...kept, user: "root" }), true);
  assert.equal(destinationChanged(kept, { ...kept, port: "22" }), true);
  assert.equal(
    DESTINATION_CHANGED_NOTE,
    "Changing the server or user forgets the remembered password. Choose Change to set one for the new destination.",
  );
  assert.equal(PASSWORD_REMEMBERED, "A password is remembered");
  assert.equal(NO_PASSWORD_REMEMBERED, "No password remembered");
});

test("shared texts with the TUI (phase 6b closing fixes)", () => {
  assert.equal(KEY_NONE_LABEL, "none · agent and default keys");
  assert.equal(INCLUDE_MESSAGE, "Optional: for ssh, scp and git to see these hosts too, add this line at the top of ~/.ssh/config:");
  assert.equal(
    READ_ONLY_IMPORTED,
    "Imported by an earlier version of Portico (imports/termius.conf), which it never edits. Duplicate it as a Portico host to change it.",
  );
  assert.equal(IMPORTED_EDIT_HELP, "Editing it keeps later imports from updating it.");
  assert.equal(originLabel("portico", "termix"), "Portico · from Termix");
  assert.equal(originLabel("portico", "termius"), "Portico · from Termius");
  assert.equal(originLabel("portico", null), "Portico");
  assert.equal(originLabel("imported", null), "Imported");
  assert.equal(cannotDeleteTitle("db-prod"), "db-prod can't be deleted here");
  const host = { alias: "ci-runner", user: "ci", hostname: "10.0.9.4", port: 22 };
  assert.equal(accountText(host), "ci@10.0.9.4:22");
  assert.equal(
    deleteConfirmationText(host),
    "Delete ci-runner (ci@10.0.9.4:22) and its tags and notes. Its remembered password is forgotten unless another host uses the same account. Open sessions stay connected. This cannot be undone.",
  );
  assert.equal(
    sendsRememberedPassword("admin", "192.168.1.20", 2222),
    "The password remembered for admin@192.168.1.20:2222 will be sent to this server once you accept its key.",
  );
  assert.equal(
    sendsRememberedPassword(null, "nas.lan", 22),
    "The password remembered for nas.lan:22 will be sent to this server once you accept its key.",
  );
});
