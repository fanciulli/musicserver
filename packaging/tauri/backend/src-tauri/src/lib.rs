// Music Server – backend desktop package.
//
// This Tauri app is a thin supervisor: it starts a bundled MongoDB instance and
// the Music Server backend (run with a bundled Node.js runtime), then keeps them
// alive. It deliberately shows no administrative UI — only a tray icon and a
// small status window. The backend API is exposed on 127.0.0.1:3000.

use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, RunEvent, WindowEvent};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

const HOST: &str = "127.0.0.1";
const MONGO_PORT: u16 = 27017;
const BACKEND_PORT: u16 = 3000;

/// Live status of the supervised services, surfaced to the status window.
#[derive(Default, Clone, Serialize)]
struct Status {
    mongo: bool,
    backend: bool,
    message: String,
}

/// Shared application state.
#[derive(Default)]
struct AppState {
    children: Mutex<Vec<CommandChild>>,
    status: Mutex<Status>,
}

#[tauri::command]
fn get_status(state: tauri::State<'_, AppState>) -> Status {
    state.status.lock().unwrap().clone()
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![get_status])
        .setup(|app| {
            build_tray(app.handle())?;

            // Start MongoDB + backend on a background thread so the UI event
            // loop is never blocked while we wait for ports to come up.
            let handle = app.handle().clone();
            thread::spawn(move || {
                if let Err(e) = start_services(&handle) {
                    set_message(&handle, &format!("Failed to start services: {e}"));
                    eprintln!("[backend-launcher] {e}");
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the status window only hides it; the services keep running.
            // Use the tray "Quit" item to stop everything.
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Music Server backend")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                kill_children(app);
            }
        });
}

// ── service supervision ──────────────────────────────────────────────────────

fn start_services(app: &AppHandle) -> Result<(), String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("cannot resolve app data dir: {e}"))?;
    let mongo_data = data_dir.join("mongodb");
    let logs = data_dir.join("logs");
    std::fs::create_dir_all(&mongo_data).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&logs).map_err(|e| e.to_string())?;

    // 1. MongoDB
    set_message(app, "Starting MongoDB…");
    let mongo_log = logs.join("mongod.log");
    let (rx, child) = app
        .shell()
        .sidecar("binaries/mongod")
        .map_err(|e| e.to_string())?
        .args([
            String::from("--dbpath"),
            path_str(&mongo_data),
            String::from("--port"),
            MONGO_PORT.to_string(),
            String::from("--bind_ip"),
            String::from(HOST),
            String::from("--logpath"),
            path_str(&mongo_log),
            String::from("--logappend"),
        ])
        .spawn()
        .map_err(|e| format!("failed to spawn mongod: {e}"))?;
    drain_output(app, "mongod", rx);
    push_child(app, child);

    if !wait_for_port(MONGO_PORT, Duration::from_secs(30)) {
        return Err("timed out waiting for MongoDB".into());
    }
    mark_mongo_up(app);

    // 2. Backend. The backend resolves runtime paths relative to its CWD, so we
    //    run it from inside its own `dist/` directory (mirrors `cd dist; node index.js`).
    set_message(app, "Starting backend…");
    let backend_dist = app
        .path()
        .resource_dir()
        .map_err(|e| format!("cannot resolve resource dir: {e}"))?
        .join("resources")
        .join("backend")
        .join("dist");

    let (rx, child) = app
        .shell()
        .sidecar("binaries/node")
        .map_err(|e| e.to_string())?
        .current_dir(backend_dist)
        .env("MONGO_URI", format!("mongodb://{HOST}:{MONGO_PORT}"))
        .args(["index.js"])
        .spawn()
        .map_err(|e| format!("failed to spawn backend: {e}"))?;
    drain_output(app, "backend", rx);
    push_child(app, child);

    if !wait_for_port(BACKEND_PORT, Duration::from_secs(60)) {
        return Err("timed out waiting for backend".into());
    }
    mark_backend_up(app);
    Ok(())
}

fn wait_for_port(port: u16, timeout: Duration) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

/// Forward a sidecar's stdout/stderr lines to the parent's stderr (for logs).
fn drain_output(app: &AppHandle, name: &'static str, mut rx: tauri::async_runtime::Receiver<CommandEvent>) {
    let _ = app;
    tauri::async_runtime::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stdout(bytes) | CommandEvent::Stderr(bytes) => {
                    eprint!("[{name}] {}", String::from_utf8_lossy(&bytes));
                }
                CommandEvent::Error(err) => eprintln!("[{name}] error: {err}"),
                CommandEvent::Terminated(payload) => {
                    eprintln!("[{name}] terminated: code={:?}", payload.code);
                }
                _ => {}
            }
        }
    });
}

// ── state helpers ────────────────────────────────────────────────────────────

fn push_child(app: &AppHandle, child: CommandChild) {
    app.state::<AppState>().children.lock().unwrap().push(child);
}

fn kill_children(app: &AppHandle) {
    let children: Vec<CommandChild> = {
        let state = app.state::<AppState>();
        let mut guard = state.children.lock().unwrap();
        std::mem::take(&mut *guard)
    };
    // Stop in reverse start order (backend before mongod).
    for child in children.into_iter().rev() {
        let _ = child.kill();
    }
}

fn update_status<F: FnOnce(&mut Status)>(app: &AppHandle, f: F) {
    let snapshot = {
        let state = app.state::<AppState>();
        let mut status = state.status.lock().unwrap();
        f(&mut status);
        status.clone()
    };
    let _ = app.emit("status-changed", snapshot);
}

fn set_message(app: &AppHandle, msg: &str) {
    update_status(app, |s| s.message = msg.to_string());
}

fn mark_mongo_up(app: &AppHandle) {
    update_status(app, |s| {
        s.mongo = true;
        s.message = "MongoDB ready".into();
    });
}

fn mark_backend_up(app: &AppHandle) {
    update_status(app, |s| {
        s.backend = true;
        s.message = format!("Backend API ready on http://{HOST}:{BACKEND_PORT}");
    });
}

// ── tray ─────────────────────────────────────────────────────────────────────

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItemBuilder::with_id("show", "Show status").build(app)?;
    let open_data = MenuItemBuilder::with_id("open_data", "Open data folder").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit Music Server backend").build(app)?;
    let menu = MenuBuilder::new(app)
        .item(&show)
        .item(&open_data)
        .item(&quit)
        .build()?;

    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Music Server Backend")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
            "open_data" => {
                if let Ok(dir) = app.path().app_data_dir() {
                    open_path(&dir);
                }
            }
            "quit" => {
                kill_children(app);
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

// ── util ─────────────────────────────────────────────────────────────────────

fn path_str(p: &PathBuf) -> String {
    p.to_string_lossy().to_string()
}

/// Reveal a folder in the platform file manager (best effort).
fn open_path(path: &PathBuf) {
    #[cfg(target_os = "windows")]
    let cmd = ("explorer", vec![path_str(path)]);
    #[cfg(target_os = "macos")]
    let cmd = ("open", vec![path_str(path)]);
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = ("xdg-open", vec![path_str(path)]);

    let _ = std::process::Command::new(cmd.0).args(cmd.1).spawn();
}
