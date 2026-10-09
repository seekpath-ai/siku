fn main() {
    // tauri-build narrows Cargo's rebuild tracking and does not watch the
    // icon files — without this, swapping icons/icon.ico reuses the stale
    // windres-compiled resource object and the exe keeps the old icon.
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/icon.icns");
    tauri_build::build()
}
