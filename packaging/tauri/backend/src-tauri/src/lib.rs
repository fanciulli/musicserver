// Music Server – backend desktop package.
//
// This Tauri app is a thin supervisor: it starts a bundled MongoDB instance and
// the Music Server backend (run with a bundled Node.js runtime), then keeps them
// alive. It deliberately shows no administrative UI — only a tray icon, a small
// status window, and a settings window (HTTPS configuration). The backend API is
// exposed on 0.0.0.0:3000 (HTTP or HTTPS depending on the saved settings).

mod settings;

use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, RunEvent, WindowEvent};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

use settings::Settings;

const HOST: &str = "127.0.0.1";

/// Live status of the supervised services, surfaced to the status window.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    mongo: bool,
    backend: bool,
    https: bool,
    backend_port: u16,
    mongo_port: u16,
    message: String,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            mongo: false,
            backend: false,
            https: true,
            backend_port: 3000,
            mongo_port: 27017,
            message: String::new(),
        }
    }
}

/// A supervised child process tagged with a stable name.
struct Managed {
    name: &'static str,
    child: CommandChild,
}

/// Shared application state.
#[derive(Default)]
struct AppState {
    children: Mutex<Vec<Managed>>,
    status: Mutex<Status>,
    settings: Mutex<Settings>,
}

// ── commands ─────────────────────────────────────────────────────────────────

#[tauri::command]
fn get_status(state: tauri::State<'_, AppState>) -> Status {
    state.status.lock().unwrap().clone()
}

#[tauri::command]
fn get_settings(state: tauri::State<'_, AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn apply_settings(app: AppHandle, new_settings: Settings) -> Result<(), String> {
    let old_mongo_port = app.state::<AppState>().settings.lock().unwrap().mongo_port;
    settings::save(&app, &new_settings)?;
    let mongo_changed = new_settings.mongo_port != old_mongo_port;
    *app.state::<AppState>().settings.lock().unwrap() = new_settings;

    // Apply on a background thread so the command returns immediately; the UI
    // tracks progress via status events. Changing the Mongo port requires
    // restarting Mongo (and the backend, since MONGO_URI changes); otherwise we
    // only restart the backend.
    let handle = app.clone();
    thread::spawn(move || {
        let result = if mongo_changed {
            restart_all(&handle)
        } else {
            restart_backend(&handle)
        };
        if let Err(e) = result {
            set_message(&handle, &format!("Restart failed: {e}"));
            eprintln!("[backend-launcher] restart failed: {e}");
        }
    });
    Ok(())
}

// ── app entry ────────────────────────────────────────────────────────────────

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_status,
            get_settings,
            apply_settings
        ])
        .setup(|app| {
            // Load persisted settings into state before anything reads them.
            let loaded = settings::load(app.handle());
            {
                let state = app.state::<AppState>();
                let mut status = state.status.lock().unwrap();
                status.https = loaded.https.enabled;
                status.backend_port = loaded.backend_port;
                status.mongo_port = loaded.mongo_port;
                *state.settings.lock().unwrap() = loaded;
            }

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
            // Closing a window only hides it; the services keep running. Use the
            // tray "Quit" item to stop everything.
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Music Server backend")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                kill_all(app);
            }
        });
}

// ── service supervision ──────────────────────────────────────────────────────

fn start_services(app: &AppHandle) -> Result<(), String> {
    let (mongo_port, mongo_exited) = spawn_mongo(app)?;
    wait_for_service(mongo_port, &mongo_exited, Duration::from_secs(30), "MongoDB")?;
    mark_mongo_up(app);

    let (backend_port, backend_exited) = spawn_backend(app)?;
    wait_for_service(backend_port, &backend_exited, Duration::from_secs(60), "backend")?;
    mark_backend_up(app);
    Ok(())
}

/// Spawn MongoDB on the configured port. Returns the port and an "exited" flag.
fn spawn_mongo(app: &AppHandle) -> Result<(u16, Arc<AtomicBool>), String> {
    let mongo_port = app.state::<AppState>().settings.lock().unwrap().mongo_port;

    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("cannot resolve app data dir: {e}"))?;
    let mongo_data = data_dir.join("mongodb");
    let logs = data_dir.join("logs");
    std::fs::create_dir_all(&mongo_data).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&logs).map_err(|e| e.to_string())?;

    set_message(app, "Starting MongoDB…");
    let mongo_log = logs.join("mongod.log");
    let command = app
        .shell()
        .sidecar("mongod")
        .map_err(|e| e.to_string())?
        .args([
            String::from("--dbpath"),
            path_str(&mongo_data),
            String::from("--port"),
            mongo_port.to_string(),
            String::from("--bind_ip"),
            String::from(HOST),
            String::from("--logpath"),
            path_str(&mongo_log),
            String::from("--logappend"),
        ]);
    let exited = spawn_logged(app, "mongod", command)?;
    Ok((mongo_port, exited))
}

/// Spawn the backend Node process using the current settings (ports + HTTPS).
/// Returns the backend port and an "exited" flag.
fn spawn_backend(app: &AppHandle) -> Result<(u16, Arc<AtomicBool>), String> {
    let current = app.state::<AppState>().settings.lock().unwrap().clone();
    let (cert, key) = settings::effective_cert_paths(app, &current)?;
    let https = current.https.enabled;
    let backend_port = current.backend_port;
    let mongo_port = current.mongo_port;

    // The backend resolves runtime paths relative to its CWD, so we run it from
    // inside its own `dist/` directory (mirrors `cd dist; node index.js`).
    let backend_dist = app
        .path()
        .resource_dir()
        .map_err(|e| format!("cannot resolve resource dir: {e}"))?
        .join("resources")
        .join("backend")
        .join("dist");

    let command = app
        .shell()
        .sidecar("node")
        .map_err(|e| e.to_string())?
        .current_dir(backend_dist)
        .env("PORT", backend_port.to_string())
        .env("MONGO_URI", format!("mongodb://{HOST}:{mongo_port}"))
        .env("HTTPS_ENABLED", if https { "true" } else { "false" })
        .env("TLS_CERT_PATH", path_str(&cert))
        .env("TLS_KEY_PATH", path_str(&key))
        .args(["index.js"]);
    let exited = spawn_logged(app, "backend", command)?;
    Ok((backend_port, exited))
}

/// Stop and re-launch just the backend to apply new settings.
fn restart_backend(app: &AppHandle) -> Result<(), String> {
    set_message(app, "Applying settings — restarting backend…");
    update_status(app, |s| s.backend = false);
    kill_named(app, "backend");

    let backend_port = app.state::<AppState>().settings.lock().unwrap().backend_port;
    wait_for_port_free(backend_port, Duration::from_secs(15));
    let (port, exited) = spawn_backend(app)?;
    wait_for_service(port, &exited, Duration::from_secs(60), "backend")?;
    mark_backend_up(app);
    Ok(())
}

/// Stop and re-launch both MongoDB and the backend (used when the Mongo port
/// changes, since the backend's MONGO_URI then changes too).
fn restart_all(app: &AppHandle) -> Result<(), String> {
    set_message(app, "Applying settings — restarting services…");
    update_status(app, |s| {
        s.mongo = false;
        s.backend = false;
    });
    kill_named(app, "backend");
    kill_named(app, "mongod");

    let (backend_port, mongo_port) = {
        let state = app.state::<AppState>();
        let s = state.settings.lock().unwrap();
        (s.backend_port, s.mongo_port)
    };
    wait_for_port_free(mongo_port, Duration::from_secs(15));
    wait_for_port_free(backend_port, Duration::from_secs(15));

    let (mongo_port, mongo_exited) = spawn_mongo(app)?;
    wait_for_service(mongo_port, &mongo_exited, Duration::from_secs(30), "MongoDB")?;
    mark_mongo_up(app);

    let (port, exited) = spawn_backend(app)?;
    wait_for_service(port, &exited, Duration::from_secs(60), "backend")?;
    mark_backend_up(app);
    Ok(())
}

// ── port helpers ─────────────────────────────────────────────────────────────

/// Wait until `port` accepts a connection, failing fast if the supervised
/// process exits first or the timeout elapses.
fn wait_for_service(
    port: u16,
    exited: &Arc<AtomicBool>,
    timeout: Duration,
    name: &str,
) -> Result<(), String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok() {
            return Ok(());
        }
        if exited.load(Ordering::SeqCst) {
            return Err(format!(
                "{name} exited before listening on port {port}; see logs/{name}.log (tray → Open data folder)"
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for {name} on port {port}; see logs/{name}.log (tray → Open data folder)"
            ));
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn wait_for_port_free(port: u16, timeout: Duration) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

/// Spawn a sidecar command, streaming its output to `<app-data>/logs/<name>.log`
/// (and the parent stderr), and returning a flag that is set when it exits.
fn spawn_logged(
    app: &AppHandle,
    name: &'static str,
    command: tauri_plugin_shell::process::Command,
) -> Result<Arc<AtomicBool>, String> {
    let (rx, child) = command
        .spawn()
        .map_err(|e| format!("failed to spawn {name}: {e}"))?;
    let exited = Arc::new(AtomicBool::new(false));
    drain_output(app, name, rx, exited.clone());
    push_child(app, name, child);
    Ok(exited)
}

/// Forward a sidecar's output to a log file + parent stderr, and flip `exited`
/// when the process terminates.
fn drain_output(
    app: &AppHandle,
    name: &'static str,
    mut rx: tauri::async_runtime::Receiver<CommandEvent>,
    exited: Arc<AtomicBool>,
) {
    let log_path = app
        .path()
        .app_data_dir()
        .ok()
        .map(|d| d.join("logs").join(format!("{name}.log")));
    tauri::async_runtime::spawn(async move {
        while let Some(event) = rx.recv().await {
            let line = match event {
                CommandEvent::Stdout(bytes) | CommandEvent::Stderr(bytes) => {
                    String::from_utf8_lossy(&bytes).into_owned()
                }
                CommandEvent::Error(err) => format!("error: {err}\n"),
                CommandEvent::Terminated(payload) => {
                    exited.store(true, Ordering::SeqCst);
                    format!("terminated: code={:?}\n", payload.code)
                }
                _ => String::new(),
            };
            if line.is_empty() {
                continue;
            }
            eprint!("[{name}] {line}");
            if let Some(ref path) = log_path {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                    use std::io::Write;
                    let _ = f.write_all(line.as_bytes());
                }
            }
        }
    });
}

// ── child + status helpers ───────────────────────────────────────────────────

fn push_child(app: &AppHandle, name: &'static str, child: CommandChild) {
    app.state::<AppState>()
        .children
        .lock()
        .unwrap()
        .push(Managed { name, child });
}

fn kill_named(app: &AppHandle, name: &str) {
    let state = app.state::<AppState>();
    let mut guard = state.children.lock().unwrap();
    let mut keep = Vec::new();
    for managed in guard.drain(..) {
        if managed.name == name {
            let _ = managed.child.kill();
        } else {
            keep.push(managed);
        }
    }
    *guard = keep;
}

fn kill_all(app: &AppHandle) {
    let children: Vec<Managed> = {
        let state = app.state::<AppState>();
        let mut guard = state.children.lock().unwrap();
        std::mem::take(&mut *guard)
    };
    // Stop in reverse start order (backend before mongod).
    for managed in children.into_iter().rev() {
        let _ = managed.child.kill();
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
    let mongo_port = app.state::<AppState>().settings.lock().unwrap().mongo_port;
    update_status(app, |s| {
        s.mongo = true;
        s.mongo_port = mongo_port;
        s.message = format!("MongoDB ready on {HOST}:{mongo_port}");
    });
}

fn mark_backend_up(app: &AppHandle) {
    let (https, backend_port) = {
        let state = app.state::<AppState>();
        let s = state.settings.lock().unwrap();
        (s.https.enabled, s.backend_port)
    };
    let scheme = if https { "https" } else { "http" };
    update_status(app, |s| {
        s.backend = true;
        s.https = https;
        s.backend_port = backend_port;
        s.message = format!("Backend API ready on {scheme}://{HOST}:{backend_port}");
    });
}

// ── tray ─────────────────────────────────────────────────────────────────────

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItemBuilder::with_id("show", "Show status").build(app)?;
    let settings_item = MenuItemBuilder::with_id("settings", "Settings…").build(app)?;
    let open_data = MenuItemBuilder::with_id("open_data", "Open data folder").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit Music Server backend").build(app)?;
    let menu = MenuBuilder::new(app)
        .item(&show)
        .item(&settings_item)
        .item(&open_data)
        .item(&quit)
        .build()?;

    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Music Server Backend")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_window(app, "main"),
            "settings" => show_window(app, "settings"),
            "open_data" => {
                if let Ok(dir) = app.path().app_data_dir() {
                    open_path(&dir);
                }
            }
            "quit" => {
                kill_all(app);
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn show_window(app: &AppHandle, label: &str) {
    if let Some(win) = app.get_webview_window(label) {
        let _ = win.show();
        let _ = win.set_focus();
    }
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
