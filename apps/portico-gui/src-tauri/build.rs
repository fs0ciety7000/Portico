//! Tauri's build step, then Windows (MSVC) link hardening: the DLLs the
//! executable imports are looked up in `System32` only
//! (`/DEPENDENTLOADFLAG:0x800`, `LOAD_LIBRARY_SEARCH_SYSTEM32`), never next
//! to it. The `WebView2` loader is linked statically on MSVC and the C runtime
//! too (`static_vcruntime`), so every import is a system DLL.

fn main() {
    tauri_build::build();
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
    }
}
