// Host list layout: without a search, "Recent" (last hosts still in the
// config), then the groups (alphabetical), then "Other"; collapsible
// sections whose headers are rows too (keyboard: ↵ / ← / → fold them).
// Without groups nor recent hosts, one flat list. With a search, the
// ranked results, flat.
import type { Host, RecentHost } from "./api";

/** Recent hosts shown in the sidebar. */
export const RECENT_SHOWN = 5;

export type SectionKind = "recent" | "group" | "other" | "flat";

export interface Section {
  /** Stable key (`recent`, `group:prod`, `other`, `flat`). */
  id: string;
  kind: SectionKind;
  title: string;
  count: number;
  collapsed: boolean;
}

export type Row =
  | { kind: "header"; key: string; section: Section }
  | { kind: "host"; key: string; section: Section; host: Host };

export function rowHost(row: Row | undefined): Host | undefined {
  return row?.kind === "host" ? row.host : undefined;
}

export function buildRows(
  hosts: Host[],
  recent: RecentHost[],
  collapsed: Record<string, boolean>,
  searching: boolean,
): Row[] {
  const flat = (title: string): Row[] => {
    const section: Section = { id: "flat", kind: "flat", title, count: hosts.length, collapsed: false };
    return hosts.map((host) => ({ kind: "host", key: `flat:${host.alias}`, section, host }));
  };
  if (searching) return flat("Results");
  const byAlias = new Map(hosts.map((h) => [h.alias, h]));
  const recentHosts = recent.flatMap((r) => byAlias.get(r.alias) ?? []).slice(0, RECENT_SHOWN);
  const groups = new Map<string, Host[]>();
  const other: Host[] = [];
  for (const h of hosts) {
    if (h.group) {
      const list = groups.get(h.group) ?? [];
      list.push(h);
      groups.set(h.group, list);
    } else other.push(h);
  }
  if (!recentHosts.length && !groups.size) return flat("Hosts");

  const rows: Row[] = [];
  const add = (id: string, kind: SectionKind, title: string, list: Host[]) => {
    if (!list.length) return;
    const section: Section = { id, kind, title, count: list.length, collapsed: !!collapsed[id] };
    rows.push({ kind: "header", key: `header:${id}`, section });
    if (section.collapsed) return;
    for (const host of list) rows.push({ kind: "host", key: `${id}:${host.alias}`, section, host });
  };
  add("recent", "recent", "Recent", recentHosts);
  const names = [...groups.keys()].sort((a, b) => a.localeCompare(b, undefined, { sensitivity: "base" }));
  for (const name of names) add(`group:${name}`, "group", name, groups.get(name) ?? []);
  add("other", "other", groups.size ? "Other" : "Hosts", other);
  return rows;
}

/**
 * Where the selected row is after the list changed under it: the same row,
 * else the same host moved to another section (an edit changed its group,
 * "Recent" appeared), else -1: the index stays, so a host removed from
 * "Recent" (x) hands the selection to the next one.
 */
export function relocate(rows: Row[], previous: Row | undefined): number {
  if (!previous) return -1;
  const same = rows.findIndex((r) => r.key === previous.key);
  if (same >= 0 || previous.kind !== "host" || previous.section.kind === "recent") return same;
  const alias = previous.host.alias;
  return rows.findIndex((r) => r.kind === "host" && r.section.kind !== "recent" && r.host.alias === alias);
}

/** Existing group names, alphabetical (suggestions in the editor). */
export function groupNames(hosts: Host[]): string[] {
  const names = new Set<string>();
  for (const h of hosts) if (h.group) names.add(h.group);
  return [...names].sort((a, b) => a.localeCompare(b, undefined, { sensitivity: "base" }));
}
