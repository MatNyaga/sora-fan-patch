// Sora for Dissidia 012 — fan patch installer (Tauri v2 shell around sorapatch-core).
// The UI (../ui) calls: release_info, start(path, outDir?), cancel, reveal(path), autotest.
// Work runs on a background thread and reports through the "progress" and "outcome" events.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::Serialize;
use sorapatch_core::{build, check_input, plan_output, Problem, Progress, Release, Stage};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

mod release {
    include!(concat!(env!("OUT_DIR"), "/release.rs"));
}

struct AppState {
    rel: Arc<Release>,
    cancel: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
}

#[derive(Serialize, Clone)]
struct ProgressMsg {
    stage: Stage,
    done: u64,
    total: u64,
}

#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Outcome {
    Done { path: String, file: String, folder: String },
    Problem { problem: Problem },
}

/// Sends progress to the UI at most every 0.5 % (and on every stage change).
struct Emit {
    app: AppHandle,
    cancel: Arc<AtomicBool>,
    last: (Option<Stage>, u64),
}

impl Progress for Emit {
    fn update(&mut self, stage: Stage, done: u64, total: u64) {
        let step = if total == 0 { 0 } else { done * 200 / total };
        if self.last != (Some(stage), step) {
            self.last = (Some(stage), step);
            let _ = self.app.emit("progress", ProgressMsg { stage, done, total });
        }
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

#[tauri::command]
fn release_info(state: State<AppState>) -> serde_json::Value {
    let m = &state.rel.manifest;
    serde_json::json!({ "title": m.title, "version": m.version, "output_name": m.output.file_name })
}

#[tauri::command]
fn start(app: AppHandle, state: State<AppState>, path: String, out_dir: Option<String>) -> Result<(), String> {
    if state.busy.swap(true, Ordering::SeqCst) {
        return Err("busy".into());
    }
    state.cancel.store(false, Ordering::SeqCst);
    let (rel, cancel, busy) = (state.rel.clone(), state.cancel.clone(), state.busy.clone());
    std::thread::spawn(move || {
        let input = PathBuf::from(&path);
        let out_dir = out_dir.map(PathBuf::from);
        let mut prog = Emit { app: app.clone(), cancel, last: (None, u64::MAX) };
        let result = check_input(&rel, &input, &mut prog)
            .and_then(|_| plan_output(&rel, &input, out_dir.as_deref()))
            .and_then(|out| build(&rel, &input, &out, &mut prog));
        let outcome = match result {
            Ok(out) => Outcome::Done {
                path: out.display().to_string(),
                file: out.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
                folder: out.parent().map(|p| p.display().to_string()).unwrap_or_default(),
            },
            Err(problem) => Outcome::Problem { problem },
        };
        busy.store(false, Ordering::SeqCst);
        let _ = app.emit("outcome", outcome);
    });
    Ok(())
}

#[tauri::command]
fn cancel(state: State<AppState>) {
    state.cancel.store(true, Ordering::SeqCst);
}

/// Opens the folder with the new file selected (Explorer / Finder / file manager).
#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    tauri_plugin_opener::reveal_item_in_dir(PathBuf::from(path)).map_err(|e| e.to_string())
}

/// Test hook (CI / headless screenshots only): SORAPATCH_AUTOTEST=<input> starts at once,
/// SORAPATCH_AUTOTEST_OUT=<dir> picks the output folder. Unset for players.
#[tauri::command]
fn autotest() -> Option<serde_json::Value> {
    let input = std::env::var("SORAPATCH_AUTOTEST").ok()?;
    Some(serde_json::json!({ "input": input, "out_dir": std::env::var("SORAPATCH_AUTOTEST_OUT").ok() }))
}

fn main() {
    let rel = Release::from_parts(release::MANIFEST, release::DELTA).expect("embedded release is invalid");
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            rel: Arc::new(rel),
            cancel: Arc::new(AtomicBool::new(false)),
            busy: Arc::new(AtomicBool::new(false)),
        })
        .invoke_handler(tauri::generate_handler![release_info, start, cancel, reveal, autotest])
        // Closing the window mid-build: cancel and wait until the worker has removed its
        // temporary file, then quit (a hard exit would leave a hidden partial file behind).
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                if state.busy.load(Ordering::SeqCst) {
                    api.prevent_close();
                    state.cancel.store(true, Ordering::SeqCst);
                    let (busy, app) = (state.busy.clone(), window.app_handle().clone());
                    std::thread::spawn(move || {
                        while busy.load(Ordering::SeqCst) {
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                        app.exit(0);
                    });
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running the app");
}
