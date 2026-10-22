// Checks of the WebGL renderer choice (src/lib/webgl.ts), run by `pnpm check`.
// Node's built-in test runner, no dependency.
import assert from "node:assert/strict";
import { test } from "node:test";
import { isSoftwareRenderer } from "../src/lib/webgl.ts";

test("CPU rasterizers are recognised", () => {
  for (const name of [
    "llvmpipe (LLVM 19.1.7, 256 bits)",
    "Mesa softpipe",
    "Google SwiftShader",
    "ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (Subzero)), SwiftShader driver)",
    "Microsoft Basic Render Driver",
  ]) {
    assert.ok(isSoftwareRenderer(name), name);
  }
});

test("GPUs and unknown renderers keep WebGL", () => {
  for (const name of [
    "AMD Radeon RX 6600 (radeonsi, navi23, LLVM 19.1.7)",
    "Mesa Intel(R) UHD Graphics 620 (KBL GT2)",
    "NVIDIA GeForce RTX 3070/PCIe/SSE2",
    "Apple M2",
    "",
  ]) {
    assert.ok(!isSoftwareRenderer(name), name);
  }
});
