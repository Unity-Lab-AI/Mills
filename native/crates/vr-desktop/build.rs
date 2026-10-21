// Tauri's build step, with the app's own commands listed so each window can be allowed exactly the ones it needs
// (capabilities/ and main.rs), and the target name the updater asks the hub for.
const COMMANDS: &[&str] = &[
    "app_info", "notify", "set_badge", "open_tools", "open_external", "host_status", "host_act", "host_setup",
    "settings_get", "settings_set", "pick_folder", "fs_list", "fs_read", "fs_write", "fs_delete", "fs_mkdir",
    "term_open", "term_write", "term_resize", "term_close", "run_start", "run_stop", "git", "proxy_start", "proxy_stop",
    "proxy_status", "update_check", "update_apply", "install_here",
];
fn main() {
    println!("cargo:rustc-env=VR_TARGET={}", std::env::var("TARGET").unwrap_or_default());
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS))).expect("tauri build");
}
