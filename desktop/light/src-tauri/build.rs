fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "startup_status", "retry_start", "open_browser", "editor_ready", "editor_close_result",
        ]),
    )).expect("desktop permissions");
}
