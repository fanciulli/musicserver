# Music Server — Tauri desktop packages

This directory builds Music Server as native desktop applications using
[Tauri v2](https://v2.tauri.app/), per
[issue #110](https://github.com/fanciulli/musicserver-backend/issues/110).

Two independent packages are produced:

| Package    | Contents                         | UI                              | Ports |
| ---------- | -------------------------------- | ------------------------------- | ----- |
| `backend`  | musicserver-backend + MongoDB    | none (system tray + status/settings windows) | API on `3000`, Mongo on `27017` (loopback) |
| `frontend` | musicserver-admin-ui             | desktop window showing the UI + Settings menu | UI on `3001` (loopback) |

The frontend connects to the backend **over the network**, so the two packages
can run on the same machine or on different machines. Both the API and the UI can
be served over **HTTPS** — see [Settings & HTTPS](#settings--https).

## Why sidecars + resources?

Neither component is a static web app:

- The **backend** is a Fastify server that needs a Node.js runtime and a running
  MongoDB.
- The **admin UI** is a Next.js app with server-side API routes; it must run as a
  Node process — it is not a static bundle that can be loaded straight into a
  webview. It is run via the project's custom `server.ts` (compiled to
  `server.js`), which adds HTTPS support and self-signed certificate generation.

So each Tauri app bundles:

- a **Node.js runtime** (and, for the backend, **mongod**) as Tauri
  [sidecars](https://v2.tauri.app/develop/sidecar/) (`src-tauri/binaries/`), and
- the **compiled application code** as Tauri resources (`src-tauri/resources/`).

The small Rust supervisor in each app (`src-tauri/src/lib.rs`) starts the
sidecars, waits for their ports, and — for the frontend — points the window at
the local UI server.

## Layout

```
packaging/tauri/
├── versions.json                 # Node / MongoDB versions + source repo refs
├── scripts/
│   └── prepare-sidecars.mjs      # builds sources, fetches runtimes, stages sidecars + resources
├── backend/                      # backend desktop package
│   ├── package.json
│   ├── dist/index.html           # tray status page
│   └── src-tauri/
│       ├── tauri.conf.json
│       ├── Cargo.toml
│       ├── capabilities/default.json
│       ├── icons/
│       └── src/{main.rs,lib.rs}
└── frontend/                     # admin UI desktop package
    ├── package.json
    ├── dist/index.html           # loading page (window is then navigated to the UI)
    └── src-tauri/ ...
```

`binaries/`, `resources/`, `target/`, `gen/` and `Cargo.lock` are generated and
git-ignored.

## Prerequisites

On the **build machine**:

- Node.js 20+ and npm
- The [Rust toolchain](https://www.rust-lang.org/tools/install) (`rustc`, `cargo`)
- The [`@tauri-apps/cli`](https://v2.tauri.app/) (installed per-app via `npm install`)
- Tauri's platform prerequisites — see
  <https://v2.tauri.app/start/prerequisites/>:
  - **Linux:** `webkit2gtk-4.1`, `libappindicator3`, `librsvg2`, `patchelf`, etc.
  - **Windows:** Microsoft Visual Studio C++ Build Tools + WebView2 (preinstalled on Win 10/11)
  - **macOS:** Xcode command-line tools
- `git`, `tar`, `unzip` (used to fetch the Node.js and MongoDB runtimes)

The first build downloads a Node.js runtime and a MongoDB build into
`packaging/tauri/.cache/` (reused on subsequent builds).

## Build

Each package is a normal Tauri project. From the package directory:

```bash
cd packaging/tauri/backend     # or packaging/tauri/frontend
npm install
npm run build
```

`npm run build` runs `tauri build`, whose `beforeBuildCommand` invokes
`prepare-sidecars.mjs` to clone/build the relevant source repo, fetch the
runtimes, and stage everything. The installers/bundles land in:

```
packaging/tauri/<app>/src-tauri/target/release/bundle/
```

### Staging without building (or for another target)

You can run the staging step on its own — handy when cross-targeting or
debugging:

```bash
# from packaging/tauri/
node scripts/prepare-sidecars.mjs --app both --target host
node scripts/prepare-sidecars.mjs --app backend  --target linux-x64
node scripts/prepare-sidecars.mjs --app frontend --target macos-arm64
```

Targets: `host | macos-arm64 | macos-x64 | linux-x64 | linux-arm64 | win-x64`.

Flags:

- `--offline` — don't `git pull` / re-download; reuse cached sources and runtimes.
- `--skip-build` — reuse already-built sources in `.work/`, just re-stage.

> **Note on cross-compilation:** `prepare-sidecars.mjs` will fetch the Node and
> MongoDB binaries for any target, but `tauri build` itself produces a bundle for
> the host platform (and architecture) it runs on. To produce installers for all
> platforms, run the build on each platform (e.g. via CI runners).

## Dev mode

```bash
cd packaging/tauri/backend
node ../scripts/prepare-sidecars.mjs --app backend --target host
npm run dev
```

(`tauri dev` does not run the `beforeBuildCommand`, so stage the sidecars once
manually first.)

## Runtime behaviour

### Backend package

- Starts `mongod` with `--dbpath <app-data>/mongodb` on `127.0.0.1:<mongoPort>`
  (default `27017`).
- Starts the backend (`node index.js` from its `dist/`) with `PORT=<backendPort>`
  and `MONGO_URI=mongodb://127.0.0.1:<mongoPort>`; the backend listens on
  `0.0.0.0:<backendPort>` (default `3000`, HTTP or HTTPS per the saved settings).
- No main window on launch — a tray icon provides **Show status**,
  **Settings…**, **Open data folder**, and **Quit**. Closing a window hides it;
  the services keep running until you quit from the tray.
- MongoDB data and logs live in the OS app-data directory for
  `org.musicserver.backend`.

### Frontend package

- Starts the admin UI (the custom `server.js`) on `127.0.0.1:3001`
  (HTTP or HTTPS depending on the saved settings).
- Points the window at `http(s)://localhost:3001` once it is ready.
- The application menu **Music Server → Settings…** configures the backend URL
  and UI HTTPS.

## Settings & HTTPS

Both packages expose configuration that **persists across reboots** as a
`settings.json` file in the per-user OS config directory:

| OS      | Path |
| ------- | ---- |
| macOS   | `~/Library/Application Support/<identifier>/settings.json` |
| Windows | `%APPDATA%\<identifier>\settings.json` |
| Linux   | `~/.config/<identifier>/settings.json` |

(`<identifier>` is `org.musicserver.backend` or `org.musicserver.frontend`.)

### Backend — Settings (tray → **Settings…**)

- **Backend API port** (default `3000`) and **MongoDB port** (default `27017`).
  Changing the backend port restarts only the backend; changing the MongoDB port
  also restarts MongoDB (the backend's `MONGO_URI` follows). If you change the
  backend port, update the frontend package's backend URL to match.
- **Serve the API over HTTPS** — **enabled by default** (a self-signed
  certificate is auto-generated and persisted on first run).
- **Certificate** and **private key** paths (PEM). Leave both empty to
  auto-generate and persist a self-signed certificate under
  `<app-data>/certs/`; or point to your own certificate.

Saving restarts only the backend (MongoDB keeps running) and applies
`HTTPS_ENABLED` / `TLS_CERT_PATH` / `TLS_KEY_PATH` to it.

### Frontend — Settings (menu → **Settings…**)

- **Backend API base URL** — defaults to `https://localhost:3000` to match the
  backend package's HTTPS-by-default. When it is `https`, the UI server
  automatically trusts a self-signed backend certificate (handled by
  `server.ts`).
- **Serve the UI over HTTPS** with an optional certificate/key (same
  auto-generate behaviour as the backend).

Saving restarts only the UI sidecar and reloads the window.

> **Self-signed certificates and the desktop window:** the embedded webview
> validates TLS like a browser. A self-signed UI certificate will trigger a
> trust warning in the desktop window; for a warning-free desktop experience use
> a certificate trusted by the OS, or leave UI HTTPS off (the loopback UI is
> local-only). Self-signed certs are most useful when the UI/API are reached from
> another machine's browser.

## Troubleshooting

Each supervised process streams its output to a log file in the app data
directory (open it from the backend tray → **Open data folder**):

```
<app-data>/logs/mongod.log
<app-data>/logs/backend.log
<app-data>/logs/ui.log
```

If a service fails to come up, the status message points at the relevant log
and the supervisor reports whether the process *exited* (crashed — check the
log) or simply *timed out*. A common cause is a configured port that nothing
ends up listening on (e.g. the backend port was changed but the admin UI still
points at the old one).

`<app-data>` is:

| OS      | Path |
| ------- | ---- |
| macOS   | `~/Library/Application Support/org.musicserver.backend/` |
| Windows | `%APPDATA%\org.musicserver.backend\` |
| Linux   | `~/.local/share/org.musicserver.backend/` |

## Versions

Runtime versions and the source repo refs are in `versions.json`. Bump them
there; the build picks them up automatically.
