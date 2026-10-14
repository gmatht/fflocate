//! Windows-only: embed `ff-gui.exe.manifest` (Common-Controls v6).
//!
//! native-windows-gui binds its raw event handlers through
//! `comctl32!SetWindowSubclass`, which v5 of the controls does not export.
//! Without the v6 manifest the loader binds v5.82 and the process dies at
//! startup with `0xC0000139` before `main` runs. No-op on other platforms.

#[cfg(windows)]
fn main() {
    println!("cargo:rerun-if-changed=ff-gui.exe.manifest");
    embed_manifest::embed_manifest(embed_manifest::new_manifest("ff-gui.exe.manifest"))
        .expect("embed ff-gui manifest");
}

#[cfg(not(windows))]
fn main() {}
