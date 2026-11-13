fn main() {
    // tauri-build only watches tauri.conf.json, but the icons are built into
    // the exe too (the taskbar shows them), so rebuild when they change.
    println!("cargo:rerun-if-changed=icons");
    // The default relay address is built in (see settings.rs).
    println!("cargo:rerun-if-env-changed=ATTUNE_DEFAULT_SERVER_URL");
    tauri_build::build()
}
