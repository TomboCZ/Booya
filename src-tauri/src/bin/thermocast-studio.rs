#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

#[derive(Default)]
struct Session {
    selected: Option<PathBuf>,
    result: Option<Output>,
    running: bool,
}

#[derive(Clone, Serialize)]
struct Output {
    video: PathBuf,
    subtitles: PathBuf,
}

async fn background<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn tutorial_profile() -> String {
    std::env::var("THERMOCAST_PROFILE").unwrap_or_else(|_| "cloud".into())
}

#[tauri::command]
async fn tutorial_import(app: AppHandle) -> Result<Option<PathBuf>, String> {
    background(move || {
        let Some(file) = app
            .dialog()
            .file()
            .add_filter("Screen recording", &["mp4", "mov"])
            .blocking_pick_file()
        else {
            return Ok(None);
        };
        let path = file.into_path()?.canonicalize()?;
        let state = app.state::<Mutex<Session>>();
        let mut session = state.lock().map_err(|_| anyhow!("session unavailable"))?;
        ensure!(!session.running, "Analysis already running");
        app.asset_protocol_scope().allow_file(&path)?;
        session.result = None;
        session.selected = Some(path.clone());
        Ok(Some(path))
    })
    .await
}

fn run_worker(app: &AppHandle, source: &Path) -> Result<Output> {
    let python_relative = if cfg!(windows) {
        ".venv/Scripts/python.exe"
    } else {
        ".venv/bin/python"
    };
    let root = match std::env::var_os("THERMOCAST_ROOT") {
        Some(path) => PathBuf::from(path),
        None => {
            let executable = std::env::current_exe()?;
            executable
                .ancestors()
                .find(|dir| {
                    dir.join("thermocast/__main__.py").is_file()
                        && dir.join(python_relative).is_file()
                })
                .context(
                    "Keep the desktop build inside the ThermoCast project and initialize its .venv",
                )?
                .to_path_buf()
        }
    };
    let python = std::env::var_os("THERMOCAST_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(python_relative));
    let output = root.join("outputs");
    std::fs::create_dir_all(&output)?;
    let mut child = booya_core::proc::command(python)
        .args(["-u", "-m", "thermocast", "run"])
        .arg(source)
        .arg("--output-dir")
        .arg(&output)
        .arg("--profile")
        .arg(tutorial_profile())
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("start analysis worker")?;

    let mut result = None;
    let mut error = None;
    let reader = BufReader::new(child.stdout.take().context("open worker output")?);
    for line in reader.lines() {
        let line = match line {
            Ok(s) => s,
            Err(e) => {
                error = Some(e.to_string());
                break;
            }
        };
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match event["type"].as_str() {
            Some("progress") => {
                let _ = app.emit("tutorial-progress", &event);
            }
            Some("error") => {
                error = Some(
                    event["message"]
                        .as_str()
                        .unwrap_or("Analysis failed")
                        .to_owned(),
                )
            }
            Some("result") => {
                if let (Some(v), Some(s)) = (event["video"].as_str(), event["subtitles"].as_str()) {
                    result = Some(Output {
                        video: v.into(),
                        subtitles: s.into(),
                    });
                }
            }
            _ => {}
        }
    }
    let status = child.wait()?;
    if let Some(error) = error {
        bail!("{error}");
    }
    ensure!(status.success(), "Analysis worker exited with {status}");
    let result = result.context("worker did not return a video")?;
    let base = output.canonicalize()?;
    for path in [&result.video, &result.subtitles] {
        ensure!(
            path.is_file() && path.canonicalize()?.starts_with(&base),
            "worker returned an invalid artifact"
        );
    }
    app.asset_protocol_scope().allow_file(&result.video)?;
    Ok(result)
}

#[tauri::command]
async fn tutorial_analyze(app: AppHandle) -> Result<Output, String> {
    background(move || {
        let state = app.state::<Mutex<Session>>();
        let source = {
            let mut session = state.lock().map_err(|_| anyhow!("session unavailable"))?;
            ensure!(!session.running, "Analysis already running");
            let path = session
                .selected
                .clone()
                .context("Import a recording first")?;
            session.running = true;
            session.result = None;
            path
        };
        let result = run_worker(&app, &source);
        let mut session = state.lock().map_err(|_| anyhow!("session unavailable"))?;
        session.running = false;
        session.result = result.as_ref().ok().cloned();
        result
    })
    .await
}

#[tauri::command]
fn tutorial_subtitles(app: AppHandle) -> Result<String, String> {
    let state = app.state::<Mutex<Session>>();
    let session = state.lock().map_err(|_| "session unavailable")?;
    let output = session.result.as_ref().ok_or("No completed tutorial")?;
    std::fs::read_to_string(&output.subtitles).map_err(|e| e.to_string())
}

#[tauri::command]
async fn tutorial_export(app: AppHandle) -> Result<Option<PathBuf>, String> {
    background(move || {
        let result = app
            .state::<Mutex<Session>>()
            .lock()
            .map_err(|_| anyhow!("session unavailable"))?
            .result
            .clone()
            .context("Analyze a recording first")?;
        let Some(folder) = app
            .dialog()
            .file()
            .set_title("Export English Tutorial to folder")
            .blocking_pick_folder()
        else {
            return Ok(None);
        };
        let folder = folder.into_path()?;
        // Stage both artifacts together, then publish the directory in one rename.
        let staged = booya_core::fsutil::sibling_tmp(&folder.join("English Tutorial"));
        std::fs::create_dir(&staged)?;
        let export = (|| -> Result<PathBuf> {
            std::fs::copy(&result.video, staged.join("English Tutorial.mp4"))?;
            std::fs::copy(&result.subtitles, staged.join("English Tutorial.srt"))?;
            for index in 1.. {
                let name = if index == 1 {
                    "English Tutorial".into()
                } else {
                    format!("English Tutorial ({index})")
                };
                let target = folder.join(name);
                if !target.exists() {
                    std::fs::rename(&staged, &target)?;
                    return Ok(target);
                }
            }
            unreachable!()
        })();
        if export.is_err() {
            let _ = std::fs::remove_dir_all(&staged);
        }
        export.map(Some)
    })
    .await
}

fn main() {
    let mut context = tauri::generate_context!();
    let config = context.config_mut();
    config.identifier = "studio.thermocast.desktop".into();
    config.product_name = Some("ThermoCast Studio".into());
    let window = &mut config.app.windows[0];
    window.title = "ThermoCast Studio".into();
    window.url = tauri::WebviewUrl::App("tutorial.html".into());
    window.min_width = Some(580.0);
    window.min_height = Some(480.0);
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Mutex::new(Session::default()))
        .invoke_handler(tauri::generate_handler![
            tutorial_profile,
            tutorial_import,
            tutorial_analyze,
            tutorial_subtitles,
            tutorial_export
        ])
        .run(context)
        .expect("Unable to start ThermoCast Studio");
}
