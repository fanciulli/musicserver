// Persistent settings for the Music Server admin UI package.
//
// Stored as JSON in the per-user app config directory (survives reboots), e.g.
//   macOS:   ~/Library/Application Support/org.musicserver.frontend/settings.json
//   Windows: %APPDATA%\org.musicserver.frontend\settings.json
//   Linux:   ~/.config/org.musicserver.frontend/settings.json

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpsSettings {
    /// Serve the admin UI over HTTPS.
    pub enabled: bool,
    /// Absolute path to a certificate (PEM). Empty = auto-generate self-signed.
    pub cert_path: String,
    /// Absolute path to the private key (PEM). Empty = auto-generate self-signed.
    pub key_path: String,
}

impl Default for HttpsSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            cert_path: String::new(),
            key_path: String::new(),
        }
    }
}

fn default_backend_url() -> String {
    // The backend package serves HTTPS by default; server.ts trusts a
    // self-signed backend certificate automatically for https URLs.
    "https://localhost:3000".to_string()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Base URL of the Music Server backend API (http or https).
    #[serde(default = "default_backend_url")]
    pub backend_url: String,
    #[serde(default)]
    pub https: HttpsSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            backend_url: default_backend_url(),
            https: HttpsSettings::default(),
        }
    }
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("cannot resolve config dir: {e}"))?;
    Ok(dir.join("settings.json"))
}

pub fn load(app: &AppHandle) -> Settings {
    match settings_path(app)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
    {
        Some(txt) => serde_json::from_str(&txt).unwrap_or_default(),
        None => Settings::default(),
    }
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let path = settings_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let txt = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, txt).map_err(|e| e.to_string())
}

/// Resolve the effective (cert, key) absolute paths for the UI's HTTPS server.
/// Empty config values fall back to a writable, persistent location under the
/// app data dir, where `server.ts` will auto-generate a self-signed cert.
pub fn effective_cert_paths(
    app: &AppHandle,
    settings: &Settings,
) -> Result<(PathBuf, PathBuf), String> {
    let default_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("cannot resolve data dir: {e}"))?
        .join("certs");

    let cert = if settings.https.cert_path.trim().is_empty() {
        default_dir.join("server.crt")
    } else {
        PathBuf::from(settings.https.cert_path.trim())
    };
    let key = if settings.https.key_path.trim().is_empty() {
        default_dir.join("server.key")
    } else {
        PathBuf::from(settings.https.key_path.trim())
    };
    Ok((cert, key))
}
