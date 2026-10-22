// Whether the terminal should use xterm.js's WebGL renderer.
//
// With WebKitGTK's DMA-BUF renderer off (Linux, see src-tauri `lib.rs`), or
// on machines without a usable GPU driver, WebGL runs on a CPU rasterizer
// (llvmpipe, SwiftShader…). There the WebGL renderer repaints far slower than
// the DOM renderer and typing lags, so it is only used on a real GPU.

const SOFTWARE = /llvmpipe|softpipe|lavapipe|swiftshader|swrast|software|basic render/i;

/** True when a WebGL renderer name designates a CPU rasterizer. */
export function isSoftwareRenderer(name: string): boolean {
  return SOFTWARE.test(name);
}

let cached: boolean | undefined;

/** True when WebGL2 is available and runs on a GPU (checked once). */
export function hardwareWebgl(): boolean {
  if (cached === undefined) cached = probe();
  return cached;
}

function probe(): boolean {
  try {
    const gl = document.createElement("canvas").getContext("webgl2");
    if (!gl) return false;
    const info = gl.getExtension("WEBGL_debug_renderer_info");
    // Unknown renderer: trust it, as before.
    const name = info ? String(gl.getParameter(info.UNMASKED_RENDERER_WEBGL) ?? "") : "";
    gl.getExtension("WEBGL_lose_context")?.loseContext();
    return !isSoftwareRenderer(name);
  } catch {
    return false;
  }
}
