// Music Server – admin UI desktop package.
//
// This Tauri app runs the Next.js admin UI (with a bundled Node.js runtime) as a
// local sidecar on 127.0.0.1:3001 using the project's custom `server.ts`, which
// supports HTTPS and auto-generates a self-signed certificate when enabled. The
// window is pointed at the running server. The UI talks to the Music Server
// backend over the network; both the backend URL and the UI's own HTTPS settings
// are configurable from the application menu and persist across reboots.

mod settings;

use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::{AppHandle, Manager, RunEvent, WindowEvent};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

use settings::Settings;

const HOST: &str = "127.0.0.1";
const UI_PORT: u16 = 3001;

struct Managed {
    name: &'static str,
    child: CommandChild,
}

#[derive(Default)]
struct AppState {
    children: Mutex<Vec<Managed>>,
    settings: Mutex<Settings>,
}

// ── commands ─────────────────────────────────────────────────────────────────

#[tauri::command]
fn get_settings(state: tauri::State<'_, AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn apply_settings(app: AppHandle, new_settings: Settings) -> Result<(), String> {
    // When UI HTTPS is enabled, both a certificate and a key are required: the
    // embedded webview cannot load a self-signed (auto-generated) certificate,
    // so a trusted cert/key pair must be provided explicitly.
    if new_settings.https.enabled
        && (new_settings.https.cert_path.trim().is_empty()
            || new_settings.https.key_path.trim().is_empty())
    {
        return Err(
            "HTTPS is enabled — provide both a certificate and a private key path \
             (the desktop webview does not accept self-signed certificates)."
                .to_string(),
        );
    }

    settings::save(&app, &new_settings)?;
    *app.state::<AppState>().settings.lock().unwrap() = new_settings;

    let handle = app.clone();
    thread::spawn(move || {
        if let Err(e) = restart_ui(&handle) {
            eprintln!("[frontend-launcher] restart failed: {e}");
        }
    });
    Ok(())
}

// ── app entry ────────────────────────────────────────────────────────────────

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![get_settings, apply_settings])
        .menu(|handle| {
            let settings_item = MenuItemBuilder::with_id("settings", "Settings…").build(handle)?;
            let quit = MenuItemBuilder::with_id("quit", "Quit").build(handle)?;
            let app_menu = SubmenuBuilder::new(handle, "Music Server Admin UI")
                .item(&settings_item)
                .separator()
                .item(&quit)
                .build()?;
            MenuBuilder::new(handle).item(&app_menu).build()
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
            "settings" => {
                if let Some(win) = app.get_webview_window("settings") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
            "quit" => {
                kill_all(app);
                app.exit(0);
            }
            _ => {}
        })
        .setup(|app| {
            let loaded = settings::load(app.handle());
            *app.state::<AppState>().settings.lock().unwrap() = loaded;

            let handle = app.handle().clone();
            thread::spawn(move || {
                if let Err(e) = start_ui(&handle) {
                    eprintln!("[frontend-launcher] {e}");
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                match window.label() {
                    // The settings window just hides so it can be reopened.
                    "settings" => {
                        let _ = window.hide();
                        api.prevent_close();
                    }
                    // Closing the main window quits the app.
                    _ => {
                        kill_all(window.app_handle());
                        window.app_handle().exit(0);
                    }
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Music Server admin UI")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                kill_all(app);
            }
        });
}

// ── UI supervision ───────────────────────────────────────────────────────────

fn start_ui(app: &AppHandle) -> Result<(), String> {
    let exited = spawn_ui(app)?;
    wait_for_service(UI_PORT, &exited, Duration::from_secs(60), "ui")?;
    navigate_to_ui(app)
}

fn spawn_ui(app: &AppHandle) -> Result<Arc<AtomicBool>, String> {
    let current = app.state::<AppState>().settings.lock().unwrap().clone();
    let (cert, key) = settings::effective_cert_paths(app, &current)?;
    let https = current.https.enabled;

    let ui_dir = app
        .path()
        .resource_dir()
        .map_err(|e| format!("cannot resolve resource dir: {e}"))?
        .join("resources")
        .join("ui");

    preflight_check(app, "ui", &ui_dir, "server.js")?;

    let command = app
        .shell()
        .sidecar("node")
        .map_err(|e| e.to_string())?
        .current_dir(ui_dir)
        .env("NODE_ENV", "production")
        .env("PORT", UI_PORT.to_string())
        .env("HOSTNAME", HOST)
        .env("MUSICSERVER_API_BASE_URL", current.backend_url.trim())
        .env("HTTPS_ENABLED", if https { "true" } else { "false" })
        .env("TLS_CERT_PATH", path_str(&cert))
        .env("TLS_KEY_PATH", path_str(&key))
        .args(["server.js"]);
    spawn_logged(app, "ui", command)
}

fn restart_ui(app: &AppHandle) -> Result<(), String> {
    kill_named(app, "ui");
    wait_for_port_free(UI_PORT, Duration::from_secs(15));
    let exited = spawn_ui(app)?;
    wait_for_service(UI_PORT, &exited, Duration::from_secs(60), "ui")?;
    navigate_to_ui(app)
}

fn navigate_to_ui(app: &AppHandle) -> Result<(), String> {
    let https = app.state::<AppState>().settings.lock().unwrap().https.enabled;
    let scheme = if https { "https" } else { "http" };
    let url: tauri::Url = format!("{scheme}://localhost:{UI_PORT}/")
        .parse()
        .map_err(|e| format!("invalid UI url: {e}"))?;
    if let Some(win) = app.get_webview_window("main") {
        win.navigate(url).map_err(|e| e.to_string())?;
    }
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
                "{name} exited before listening on port {port}; see logs/{name}.log"
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for {name} on port {port}; see logs/{name}.log"
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
    app.state::<AppState>()
        .children
        .lock()
        .unwrap()
        .push(Managed { name, child });
    Ok(exited)
}

/// Append a line to `<app-data>/logs/<name>.log` (and parent stderr).
fn log_line(app: &AppHandle, name: &str, msg: &str) {
    eprintln!("[{name}] {msg}");
    if let Ok(dir) = app.path().app_data_dir() {
        let path = dir.join("logs").join(format!("{name}.log"));
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            use std::io::Write;
            let _ = writeln!(f, "{msg}");
        }
    }
}

/// Verify the bundled resource entry exists before spawning, logging the resolved
/// path and (on failure) what the directory actually contains.
fn preflight_check(app: &AppHandle, name: &'static str, dir: &PathBuf, entry: &str) -> Result<(), String> {
    log_line(app, name, &format!("resource dir: {}", dir.display()));
    let target = dir.join(entry);
    if target.exists() {
        return Ok(());
    }
    let listing = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<Vec<_>>()
            .join(", "),
        Err(e) => format!("<cannot read dir: {e}>"),
    };
    let msg = format!(
        "FATAL: bundled resource '{entry}' not found at {} — directory contains: [{listing}]. \
         The app was likely built without its resources; rebuild with `npm run build`.",
        target.display()
    );
    log_line(app, name, &msg);
    Err(msg)
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

// ── child helpers ────────────────────────────────────────────────────────────

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
    for managed in children.into_iter().rev() {
        let _ = managed.child.kill();
    }
}

fn path_str(p: &PathBuf) -> String {
    p.to_string_lossy().to_string()
}
