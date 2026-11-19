#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
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

#[derive(Clone, Serialize, Deserialize)]
struct WorkerArtifacts {
    video: PathBuf,
    subtitles: PathBuf,
    project: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
struct Editing {
    lines: Vec<String>,
    protected_ranges: Vec<[f64; 2]>,
    source_duration: f64,
}

#[derive(Deserialize)]
struct ProjectSummary {
    sha256: String,
    editing: Editing,
}

#[derive(Clone, Serialize)]
struct Output {
    #[serde(flatten)]
    files: WorkerArtifacts,
    editing: Editing,
    source_sha256: String,
    profile: String,
}

#[derive(Deserialize, Serialize)]
struct Corrections {
    lines: Vec<String>,
    protected_ranges: Vec<[f64; 2]>,
}

#[derive(Serialize)]
struct EditRequest {
    #[serde(flatten)]
    changes: Corrections,
    source_sha256: String,
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

fn worker_paths() -> Result<(PathBuf, PathBuf)> {
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
    Ok((root, python))
}

fn worker(app: &AppHandle, args: Vec<OsString>, edits: Option<EditRequest>) -> Result<Value> {
    let (root, python) = worker_paths()?;
    let mut command = booya_core::proc::command(python);
    command
        .args(["-u", "-m", "thermocast"])
        .args(args)
        .current_dir(root);
    if edits.is_some() {
        command.arg("--edits-stdin").stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("start analysis worker")?;

    let mut result = None;
    let mut error = if let Some(request) = edits {
        let pipe = child.stdin.take().context("open worker edits pipe")?;
        serde_json::to_writer(pipe, &request)
            .err()
            .map(|e| format!("send corrections: {e}"))
    } else {
        None
    };
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
                result = Some(event);
            }
            _ => {}
        }
    }
    let status = child.wait()?;
    if let Some(error) = error {
        bail!("{error}");
    }
    ensure!(status.success(), "Analysis worker exited with {status}");
    result.context("Worker returned no result")
}

fn prepare_output(app: &AppHandle, event: Value, profile: String) -> Result<Output> {
    let result: WorkerArtifacts = serde_json::from_value(event)?;
    let (root, _) = worker_paths()?;
    let base = root
        .join(
            std::env::var_os("THERMOCAST_CACHE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".cache/recordings")),
        )
        .canonicalize()?;
    for path in [&result.video, &result.subtitles, &result.project] {
        ensure!(
            path.is_file() && path.canonicalize()?.starts_with(&base),
            "worker returned an invalid artifact"
        );
    }
    app.asset_protocol_scope().allow_file(&result.video)?;
    let project: ProjectSummary = serde_json::from_reader(std::fs::File::open(&result.project)?)
        .context("read editable tutorial project")?;
    Ok(Output {
        files: result,
        editing: project.editing,
        source_sha256: project.sha256,
        profile,
    })
}

#[tauri::command]
async fn tutorial_analyze(app: AppHandle, changes: Option<Corrections>) -> Result<Output, String> {
    background(move || {
        let state = app.state::<Mutex<Session>>();
        let (source, edits, profile) = {
            let mut session = state.lock().map_err(|_| anyhow!("session unavailable"))?;
            ensure!(!session.running, "Analysis already running");
            let path = session
                .selected
                .clone()
                .context("Import a recording first")?;
            let edits = match changes {
                Some(changes) => {
                    let last = session
                        .result
                        .as_ref()
                        .context("Analyze a recording before editing")?;
                    Some(EditRequest {
                        changes,
                        source_sha256: last.source_sha256.clone(),
                    })
                }
                None => None,
            };
            session.running = true;
            let profile = session
                .result
                .as_ref()
                .map(|r| r.profile.clone())
                .unwrap_or_else(tutorial_profile);
            (path, edits, profile)
        };
        let args = vec![
            "run".into(),
            source.into_os_string(),
            "--output-dir".into(),
            "outputs".into(),
            "--preview".into(),
            "--profile".into(),
            profile.clone().into(),
        ];
        let result = worker(&app, args, edits).and_then(|v| prepare_output(&app, v, profile));
        let mut session = state.lock().map_err(|_| anyhow!("session unavailable"))?;
        session.running = false;
        if let Ok(output) = &result {
            session.result = Some(output.clone());
        }
        result
    })
    .await
}

#[tauri::command]
fn tutorial_subtitles(app: AppHandle) -> Result<String, String> {
    let state = app.state::<Mutex<Session>>();
    let session = state.lock().map_err(|_| "session unavailable")?;
    let output = session.result.as_ref().ok_or("No completed tutorial")?;
    std::fs::read_to_string(&output.files.subtitles).map_err(|e| e.to_string())
}

#[tauri::command]
async fn tutorial_cache(app: AppHandle) -> Result<Value, String> {
    background(move || worker(&app, vec!["cache".into(), "list".into()], None)).await
}

#[tauri::command]
async fn tutorial_resume(app: AppHandle, id: String) -> Result<Value, String> {
    background(move || {
        let binding = app.state::<Mutex<Session>>();
        {
            let mut session = binding.lock().map_err(|_| anyhow!("session unavailable"))?;
            ensure!(!session.running, "Analysis already running");
            session.running = true;
        }
        let result = worker(&app, vec!["cache".into(), "resume".into(), id.into()], None).and_then(
            |event| {
                let source = event["source"].as_str().map(PathBuf::from);
                let profile = event["profile"]
                    .as_str()
                    .context("Missing profile")?
                    .to_owned();
                Ok((prepare_output(&app, event, profile)?, source))
            },
        );
        let mut session = binding.lock().map_err(|_| anyhow!("session unavailable"))?;
        session.running = false;
        let (out, source) = result?;
        session.selected = source.clone();
        session.result = Some(out.clone());
        Ok(serde_json::json!({"result":out, "source":source}))
    })
    .await
}

#[tauri::command]
async fn tutorial_restore(app: AppHandle, id: String, profile: String) -> Result<Value, String> {
    background(move || {
        let state = app.state::<Mutex<Session>>();
        let mut session = state.lock().map_err(|_| anyhow!("session unavailable"))?;
        ensure!(!session.running, "Analysis already running");
        let event = worker(
            &app,
            vec![
                "cache".into(),
                "open".into(),
                id.into(),
                "--profile".into(),
                profile.clone().into(),
            ],
            None,
        )?;
        let source = event["source"].as_str().map(PathBuf::from);
        let out = prepare_output(&app, event, profile)?;
        session.result = Some(out.clone());
        session.selected = source.clone();
        Ok(serde_json::json!({"result":out, "source":source}))
    })
    .await
}

#[tauri::command]
async fn tutorial_clear(app: AppHandle, id: String, scope: String) -> Result<Value, String> {
    background(move || {
        let state = app.state::<Mutex<Session>>();
        let mut session = state.lock().map_err(|_| anyhow!("session unavailable"))?;
        ensure!(!session.running, "Analysis already running");
        let event = worker(
            &app,
            vec![
                "cache".into(),
                "clear".into(),
                id.clone().into(),
                "--scope".into(),
                scope.clone().into(),
            ],
            None,
        )?;
        if scope == "all"
            && session
                .result
                .as_ref()
                .is_some_and(|r| r.source_sha256 == id)
        {
            session.result = None;
            session.selected = None;
        }
        Ok(event)
    })
    .await
}

#[tauri::command]
async fn tutorial_export(app: AppHandle, formats: Vec<String>) -> Result<Option<PathBuf>, String> {
    background(move || {
        let project = app
            .state::<Mutex<Session>>()
            .lock()
            .map_err(|_| anyhow!("session unavailable"))?
            .result
            .as_ref()
            .context("Open a tutorial first")?
            .files
            .project
            .clone();
        ensure!(!formats.is_empty(), "Choose an export format");
        let Some(folder) = app
            .dialog()
            .file()
            .set_title("Export English Tutorial to folder")
            .blocking_pick_folder()
        else {
            return Ok(None);
        };
        let mut args = vec![
            "export".into(),
            "--project".into(),
            project.into_os_string(),
            "--output-dir".into(),
            folder.into_path()?.into_os_string(),
        ];
        for format in formats {
            args.extend(["--format".into(), format.into()]);
        }
        let result = worker(&app, args, None)?;
        Ok(Some(PathBuf::from(
            result["directory"]
                .as_str()
                .context("Missing export directory")?,
        )))
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
            tutorial_export,
            tutorial_cache,
            tutorial_resume,
            tutorial_restore,
            tutorial_clear
        ])
        .run(context)
        .expect("Unable to start ThermoCast Studio");
}
