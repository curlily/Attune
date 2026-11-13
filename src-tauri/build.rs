fn main() {
    // tauri-build only watches tauri.conf.json, but the icons are built into
    // the exe too (the taskbar shows them), so rebuild when they change.
    println!("cargo:rerun-if-changed=icons");
    tauri_build::build()
}
