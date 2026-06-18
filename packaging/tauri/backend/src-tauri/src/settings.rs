// Persistent settings for the Music Server backend package.
//
// Stored as JSON in the per-user app config directory (survives reboots), e.g.
//   macOS:   ~/Library/Application Support/org.musicserver.backend/settings.json
//   Windows: %APPDATA%\org.musicserver.backend\settings.json
//   Linux:   ~/.config/org.musicserver.backend/settings.json

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpsSettings {
    /// Serve the backend API over HTTPS.
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

fn default_backend_port() -> u16 {
    3000
}

fn default_mongo_port() -> u16 {
    27017
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// TCP port the backend (Fastify) API listens on.
    #[serde(default = "default_backend_port")]
    pub backend_port: u16,
    /// TCP port the bundled MongoDB listens on (loopback).
    #[serde(default = "default_mongo_port")]
    pub mongo_port: u16,
    #[serde(default)]
    pub https: HttpsSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            backend_port: default_backend_port(),
            mongo_port: default_mongo_port(),
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

/// Load settings, falling back to defaults if the file is missing or invalid.
pub fn load(app: &AppHandle) -> Settings {
    match settings_path(app)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
    {
        Some(txt) => serde_json::from_str(&txt).unwrap_or_default(),
        None => Settings::default(),
    }
}

/// Persist settings to disk.
pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let path = settings_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let txt = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, txt).map_err(|e| e.to_string())
}

/// Resolve the effective (cert, key) absolute paths. Empty config values fall
/// back to a writable, persistent location under the app data dir, where the
/// backend will auto-generate (and reuse) a self-signed certificate.
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
