fn main() {
    println!("cargo:rerun-if-changed=../web");
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=AutoTappo.exe.manifest");

    // Embed custom manifest: requireAdministrator + PerMonitorV2 DPI + Common-Controls v6.
    // Reason (see AGENTS.md 2.4 section 7): the official pet runs elevated,
    // so directed click delivery needs the same integrity level.
    let windows =
        tauri_build::WindowsAttributes::new().app_manifest(include_str!("AutoTappo.exe.manifest"));

    let attrs = tauri_build::Attributes::new().windows_attributes(windows);
    tauri_build::try_build(attrs).expect("failed to run tauri-build");
}
