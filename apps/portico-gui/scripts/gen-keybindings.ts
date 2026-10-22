// Generates the GUI part of docs/KEYBINDINGS.md (between the
// `<!-- BEGIN GENERATED: gui -->` / `<!-- END GENERATED: gui -->` markers)
// from the command table (src/lib/shortcuts.ts) and the fixed keys
// (src/lib/keybindings-doc.ts). Everything outside the markers is kept.
//
//   pnpm gen:keybindings      rewrite the block
//   pnpm check:keybindings    fail when the block is stale (CI)
//
// Runs on Node ≥ 22.18 (TypeScript type stripping), no dependency.
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { GUI_FIXED } from "../src/lib/keybindings-doc.ts";
import { ACTIONS, defaultCombos, formatCombo, parseCombo, type ActionDef, type Platform } from "../src/lib/shortcuts.ts";

const BEGIN = "<!-- BEGIN GENERATED: gui -->";
const END = "<!-- END GENERATED: gui -->";
const docPath = resolve(dirname(fileURLToPath(import.meta.url)), "../../../docs/KEYBINDINGS.md");

/** Markdown table cell: escaped pipes, code spans. */
const cell = (text: string) => text.replaceAll("|", "\\|");

function keys(action: ActionDef, platform: Platform): string {
  const combos = defaultCombos(action, platform).flatMap((t) => {
    const c = parseCombo(t);
    if (!c) throw new Error(`${action.id}: unreadable default ${t}`);
    return [`\`${formatCombo(c, platform, action.scope)}\``];
  });
  return combos.length ? combos.join(", ") : "—";
}

function actionTable(actions: ActionDef[]): string[] {
  const out = ["| Action | Identifiant | macOS | Linux |", "|---|---|---|---|"];
  for (const a of actions) {
    const id = a.fixed ? `\`${a.id}\` (fixe)` : `\`${a.id}\``;
    out.push(`| ${cell(a.doc)} | ${id} | ${cell(keys(a, "mac"))} | ${cell(keys(a, "linux"))} |`);
  }
  return out;
}

export function render(): string {
  // Tabs 2…8 add nothing to the doc: one line stands for the nine.
  const shown = ACTIONS.filter((a) => !/^tab\.goto-[2-9]$/.test(a.id)).map((a) =>
    a.id === "tab.goto-1"
      ? { ...a, doc: "Aller à l'onglet n (`tab.goto-1` … `tab.goto-9`)", defaults: { mac: ["Mod+1"], linux: ["Alt+1"] } }
      : a,
  );
  const lines: string[] = [
    BEGIN,
    "<!-- Généré par `pnpm gen:keybindings` (apps/portico-gui/src/lib/shortcuts.ts et keybindings-doc.ts) : ne pas modifier à la main. -->",
    "",
    "## Application et onglets (GUI)",
    "",
    "Actions globales, actives partout (terminal compris). Sur Linux, `Ctrl-Shift-…` est affiché parce qu'il marche partout ;",
    "dans la liste des hôtes, `Ctrl-…` seul suffit (`Ctrl-K`, `Ctrl-D`). Un « — » : pas de touche par défaut, l'action reste",
    "dans la palette et peut recevoir une combinaison dans `settings.toml`.",
    "",
    ...actionTable(shown.filter((a) => a.scope === "global")),
    "",
    "## Panneaux (GUI)",
    "",
    "Actives dans un onglet terminal (division, focus, taille, zoom, fermeture ; voir « Panneaux (GUI) : principe » plus haut). Sur Linux,",
    "`Ctrl-Shift-…` comme pour les actions globales ; les flèches passent par `Ctrl-Alt` (`⌘⌥` sur macOS).",
    "",
    ...actionTable(shown.filter((a) => a.scope === "terminal")),
    "",
    "## Liste des hôtes (GUI)",
    "",
    "Actives quand la liste des hôtes est affichée et que le focus n'est pas dans un champ (la recherche garde ses touches).",
    "",
    ...actionTable(shown.filter((a) => a.scope === "hosts")),
  ];
  for (const t of GUI_FIXED) {
    lines.push("", `## ${t.title}`, "");
    if (t.intro) lines.push(t.intro, "");
    lines.push("| Action | Touches |", "|---|---|");
    for (const [action, k] of t.rows) lines.push(`| ${cell(action)} | ${cell(k)} |`);
    if (t.outro) lines.push("", t.outro);
  }
  lines.push(END);
  return lines.join("\n");
}

function main() {
  const check = process.argv.includes("--check");
  const doc = readFileSync(docPath, "utf8");
  const begin = doc.indexOf(BEGIN);
  const end = doc.indexOf(END);
  if (begin < 0 || end < begin) {
    console.error(`docs/KEYBINDINGS.md: markers ${BEGIN} … ${END} not found`);
    process.exit(1);
  }
  const next = doc.slice(0, begin) + render() + doc.slice(end + END.length);
  if (next === doc) {
    if (check) console.log("docs/KEYBINDINGS.md is up to date");
    return;
  }
  if (check) {
    console.error("docs/KEYBINDINGS.md is stale: run `pnpm gen:keybindings` in apps/portico-gui");
    process.exit(1);
  }
  writeFileSync(docPath, next);
  console.log("docs/KEYBINDINGS.md: GUI block regenerated");
}

main();
