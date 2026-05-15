# Music Server – Single Redistributable Packaging

**Date:** 2026-05-15
**Author:** Claude (for michelangelo@volumio.org)
**Scope:** Bundle the three Music Server components (backend, admin UI, MongoDB) into a single per-platform redistributable for Windows, Linux, macOS.

---

## 1. Goals

1. End-user downloads one archive per platform, unzips, double-clicks one binary, browser opens to the admin UI.
2. No prerequisite installs on the target machine (no Node, no MongoDB, no Docker).
3. Reproducible build from a single command on a developer machine.
4. Backend and UI source repos remain unmodified — packaging lives in this repo.

## 2. Non-Goals

- Code-signing / notarization (Windows SmartScreen and macOS Gatekeeper will warn). Out of scope; can be layered later.
- Auto-update mechanism. Out of scope.
- A literal single `.exe` file. See §3.

## 3. "Single Executable" – Interpretation

A literal one-file bundle is not feasible without harmful trade-offs:

- The admin UI is a Next.js 16 app with server-side API routes (`src/app/api/{plugins,scan,logs}/route.ts`) and `output: "standalone"`. It needs a Node runtime + a filesystem layout (`.next/`, `public/`).
- MongoDB ships as a large native binary (~100 MB per platform) that expects a real data directory and cannot be embedded inside a Node process.

We therefore ship **one launcher executable per platform** plus a sibling tree of runtime files. From the user's perspective it's still "unzip + double-click one binary." The launcher is the only entry point users interact with.

## 4. Architecture

```
musicserver-<os>-<arch>/
├── musicserver(.exe)                 ← launcher binary (the thing users run)
├── runtime/
│   ├── node(.exe)                    ← bundled Node.js 22 LTS
│   └── mongod(.exe) (+ dylibs/dll)   ← bundled MongoDB 7
├── app/
│   ├── backend/
│   │   ├── dist/                     ← compiled Fastify backend (TS → JS)
│   │   └── node_modules/             ← prod-only deps
│   └── ui/
│       ├── server.js                 ← Next.js standalone entry
│       ├── .next/                    ← built UI assets
│       ├── public/
│       └── node_modules/             ← prod-only deps (minimal, from standalone)
└── data/                             ← created on first run
    ├── mongodb/                      ← mongod --dbpath
    └── logs/                         ← backend + launcher logs
```

### 4.1 Launcher responsibilities

The launcher is a small Node script bundled with `node --experimental-sea-config` (Node Single Executable Application). It:

1. Resolves all paths **relative to its own executable location** (so the bundle is fully relocatable).
2. Creates `data/mongodb/` and `data/logs/` if missing.
3. Spawns `runtime/mongod` with:
   - `--dbpath <bundle>/data/mongodb`
   - `--port 27017`
   - `--bind_ip 127.0.0.1`
   - `--logpath <bundle>/data/logs/mongod.log`
4. Waits for MongoDB to accept TCP on 127.0.0.1:27017 (poll, max 30 s).
5. Spawns backend: `runtime/node app/backend/dist/index.js`
   - `cwd = app/backend` (so its `logs/` relative writes land inside `app/backend/logs/` — see §4.4)
   - `env.MONGO_URI = mongodb://127.0.0.1:27017`
6. Waits for backend on 127.0.0.1:3000.
7. Spawns UI: `runtime/node app/ui/server.js`
   - `cwd = app/ui`
   - `env.PORT = 3001`
   - `env.HOSTNAME = 127.0.0.1`
   - `env.MUSICSERVER_API_BASE_URL = http://127.0.0.1:3000`
8. Waits for UI on 127.0.0.1:3001.
9. Opens default browser at `http://localhost:3001` (platform-specific: `open`, `xdg-open`, `start`).
10. On `SIGINT`/`SIGTERM` (and Windows console close): sends SIGTERM to children in reverse order (ui → backend → mongod), waits up to 10 s, then SIGKILL.
11. On child crash: log to `data/logs/launcher.log` and exit non-zero so users see the failure window.

### 4.2 Ports

Hardcoded 27017 / 3000 / 3001 in this first cut. If a port is busy the launcher logs and exits with a clear error. Configurable ports are a future enhancement.

### 4.3 Backend port

Backend hardcodes `port: 3000` in `src/server/musicServer.ts`. We leave that as-is (no source patch). The launcher just doesn't pass any port override.

### 4.4 Logging

- Backend writes to `logs/` relative to its `cwd`. Launcher sets `cwd = app/backend` so logs accumulate under the bundle. Acceptable; not a perfect "single data dir" but avoids patching backend source.
- MongoDB → `data/logs/mongod.log`.
- Launcher itself → `data/logs/launcher.log`.

## 5. Build pipeline

A single Node script `packaging/build.mjs` orchestrates everything. Usage:

```
node packaging/build.mjs --target macos-arm64
node packaging/build.mjs --target linux-x64
node packaging/build.mjs --target win-x64
node packaging/build.mjs --target host        # detect current platform
```

Steps per target:

1. **Source sync** – `git clone` (or `git pull`) the backend and UI repos into `packaging/.work/src/{backend,ui}`. Pinned to `main` for now; commit SHA recorded in `data/build-info.txt` of the output.
2. **Backend build** – `npm ci && npm run build` (runs `tsc`). Then prune to prod deps: copy `dist/` and run `npm ci --omit=dev` into a staging dir.
3. **UI build** – `npm ci && npm run build`. Next.js standalone output lives at `.next/standalone/` plus `.next/static/` and `public/` that must be copied alongside (per Next.js docs).
4. **Runtime fetch** – download Node.js 22 LTS and MongoDB 7 community binaries for the target. Cached under `packaging/.cache/`.
5. **Launcher build** – compile `packaging/launcher/launcher.mjs` into a SEA blob and inject into a copy of the downloaded `node` binary. Output: `musicserver(.exe)`.
6. **Assemble** – copy launcher, runtime, backend, ui into `packaging/dist/musicserver-<target>/`.
7. **Archive** – produce `musicserver-<target>.zip` (Windows) or `.tar.gz` (Linux, macOS).

### 5.1 Download URLs

- Node.js: `https://nodejs.org/dist/v22.x/node-v22.x-<platform>.<ext>`
- MongoDB: `https://fastdl.mongodb.org/<platform>/mongodb-<platform>-7.x.tgz` (or `.zip` for Windows)

Versions pinned in `packaging/versions.json` so reruns are reproducible.

### 5.2 Cross-compilation

We do **not** cross-build the launcher. The launcher is bundled by combining a *target-platform* `node` binary with a SEA blob — that combination can be assembled on any host (Linux can produce a Windows musicserver.exe, etc.) because Node provides prebuilt binaries for every target and the SEA injection step is just byte-level patching with `postject`.

Caveat: macOS launchers built on Linux/Windows will not be code-signed; macOS Gatekeeper will flag them. For an official release, run the macOS build on a Mac. The build script logs a warning when host ≠ target for macOS.

## 6. Component changes

None to backend or UI source. Everything packaging-related lives in `/packaging/` in this repo.

## 7. Open risks

- **MongoDB binary size.** Final archive is ~250 MB compressed. Acceptable for a first release.
- **Antivirus false-positives on Windows** for unsigned SEA-injected node.exe. Document the warning in the README; signing is a follow-up.
- **macOS Gatekeeper.** Same as above; users will need to right-click → Open the first time.
- **First-run latency.** MongoDB cold start + Next.js standalone warm-up adds 5-15 s. Launcher prints progress to a console window so users see activity.

## 8. Out of scope (future)

- Tray icon / start-minimised.
- Auto-update.
- Code signing and notarization.
- Replacing MongoDB with FerretDB/SQLite to shrink bundle.
- Configurable ports.
