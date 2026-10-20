// Sample data so the UI can be designed in a plain browser (`pnpm dev`)
// without the Tauri runtime. Never used inside the desktop app.
import type { Host, ThemeInfo } from "./api";

const base = { identityFiles: [], localForwards: [], remoteForwards: [], warnings: [], group: null };

export const demoHosts: Host[] = [
  { ...base, alias: "db-prod", hostname: "10.0.4.12", user: "postgres", port: 22, proxyJump: ["bastion"], tags: ["prod", "db"], favorite: true, source: "~/.ssh/config:12" },
  { ...base, alias: "bastion", hostname: "bastion.example.com", user: "ops", port: 2222, proxyJump: [], tags: ["infra"], favorite: false, source: "~/.ssh/config:1" },
  { ...base, alias: "web-01", hostname: "web-01.internal", user: "deploy", port: 22, proxyJump: ["bastion"], tags: ["prod", "web"], favorite: false, source: "~/.ssh/config:5", localForwards: ["8080 localhost:80"] },
  { ...base, alias: "staging", hostname: "staging.example.com", user: "deploy", port: 22, proxyJump: [], tags: ["staging"], favorite: false, source: "~/.ssh/conf.d/staging:1" },
];

export const demoThemes: ThemeInfo[] = [
  { id: "obsidian", name: "Obsidian", description: "Near-black mineral surfaces, marble text, brushed-brass accent." },
  { id: "basalt", name: "Basalt", description: "Cool volcanic grey-blue, steel accent." },
  { id: "travertine", name: "Travertine Night", description: "Warm dark stone, terracotta accent." },
];
