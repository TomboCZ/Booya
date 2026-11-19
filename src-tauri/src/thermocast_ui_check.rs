//! Exercise the installed desktop UI, native IPC and worker on an isolated recording.
use tauri::{webview::PageLoadEvent, Builder, Listener, Wry};

pub fn configure(
    builder: Builder<Wry>,
    window: &mut tauri::utils::config::WindowConfig,
) -> Builder<Wry> {
    if !std::env::args().any(|arg| arg == "--check-interface") {
        return builder;
    }
    let root = std::env::var_os("THERMOCAST_ROOT")
        .and_then(|p| std::path::PathBuf::from(p).canonicalize().ok());
    let temporary = std::env::temp_dir()
        .canonicalize()
        .expect("temporary directory");
    assert!(
        root.as_ref()
            .is_some_and(|p| p.parent() == Some(temporary.as_path())
                && p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .starts_with("thermocast-check-")),
        "desktop check requires a dedicated temporary workspace"
    );
    window.background_throttling = Some(tauri::utils::config::BackgroundThrottlingPolicy::Disabled);
    window.visible = false;
    window.focus = false;
    builder
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Prohibited);
            app.listen("desktop-check-note", |event| {
                eprintln!("UI check: {}", event.payload())
            });
            let handle = app.handle().clone();
            app.listen("desktop-check-result", move |event| {
                println!("Desktop interface check: {}", event.payload());
                handle.exit(if event.payload() == r#""passed""# {
                    0
                } else {
                    1
                });
            });
            Ok(())
        })
        .on_page_load(|webview, event| {
            if event.event() == PageLoadEvent::Finished && webview.label() == "main" {
                webview
                    .eval(include_str!("thermocast_ui_check.js"))
                    .expect("start native interface check");
            }
        })
}
