// Music Server – admin UI desktop package.
//
// This Tauri app runs the Next.js admin UI (with a bundled Node.js runtime) as a
// local sidecar on 127.0.0.1:3001 and points the application window at it. The UI
// talks to the Music Server backend over the network; the backend base URL is
// configurable via the MUSICSERVER_API_BASE_URL environment variable
// (default: http://localhost:3000).

use std::net::{SocketAddr, TcpStream};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, RunEvent};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

const HOST: &str = "127.0.0.1";
const UI_PORT: u16 = 3001;
const DEFAULT_BACKEND_URL: &str = "http://localhost:3000";

#[derive(Default)]
struct AppState {
    children: Mutex<Vec<CommandChild>>,
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState::default())
        .setup(|app| {
            let handle = app.handle().clone();
            thread::spawn(move || {
                if let Err(e) = start_ui(&handle) {
                    eprintln!("[frontend-launcher] {e}");
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Music Server admin UI")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                kill_children(app);
            }
        });
}

fn start_ui(app: &AppHandle) -> Result<(), String> {
    let ui_dir = app
        .path()
        .resource_dir()
        .map_err(|e| format!("cannot resolve resource dir: {e}"))?
        .join("resources")
        .join("ui");

    let backend_url =
        std::env::var("MUSICSERVER_API_BASE_URL").unwrap_or_else(|_| DEFAULT_BACKEND_URL.to_string());

    let (rx, child) = app
        .shell()
        .sidecar("binaries/node")
        .map_err(|e| e.to_string())?
        .current_dir(ui_dir)
        .env("PORT", UI_PORT.to_string())
        .env("HOSTNAME", HOST)
        .env("MUSICSERVER_API_BASE_URL", backend_url)
        .args(["server.js"])
        .spawn()
        .map_err(|e| format!("failed to spawn UI server: {e}"))?;
    drain_output(app, "ui", rx);
    app.state::<AppState>().children.lock().unwrap().push(child);

    if !wait_for_port(UI_PORT, Duration::from_secs(60)) {
        return Err("timed out waiting for the UI server".into());
    }

    // Point the window at the now-running UI server.
    let url: tauri::Url = format!("http://localhost:{UI_PORT}/")
        .parse()
        .map_err(|e| format!("invalid UI url: {e}"))?;
    if let Some(win) = app.get_webview_window("main") {
        win.navigate(url).map_err(|e| e.to_string())?;
    }
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

fn kill_children(app: &AppHandle) {
    let children: Vec<CommandChild> = {
        let state = app.state::<AppState>();
        let mut guard = state.children.lock().unwrap();
        std::mem::take(&mut *guard)
    };
    for child in children.into_iter().rev() {
        let _ = child.kill();
    }
}
