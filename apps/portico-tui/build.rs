//! Windows (MSVC) link hardening for `portico.exe`: the DLLs it imports are
//! looked up in `System32` only (`/DEPENDENTLOADFLAG:0x800`,
//! `LOAD_LIBRARY_SEARCH_SYSTEM32`), never next to the executable, so a DLL
//! planted beside it (a Downloads folder) is not loaded. Nothing on other
//! targets (mingw's `ld` has no such flag).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
    }
}
