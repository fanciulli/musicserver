# Music Server

This repo is the entry point for the Music Server project, a Typescript native, plugin based Music Server.

The scope of this repo is to give you the necessary information for using the software and possibly contribute to it.

## Repositories

- Backend: This repository contains the core part of the project, the only one you really need. Repo: https://github.com/fanciulli/musicserver-backend
- Admin-Ui: This repository contains an administrative web interface for some basic configuration tasks. Repo: https://github.com/fanciulli/musicserver-admin-ui
- Volumio Plugin: This repository contains a plugin for Volumio. The plugin allows connecting Volumio to the Music Server via its APIs. Repo: https://github.com/fanciulli/musicserver-volumio-plugin

## Dependencies

Music Server requires a MongoDB server for storing data. If you already have an instance you can confiugre Music Server to connect to it otherwise you can run a new one. The `docker-compose.yml` file in this repository will start one for you.

## Quick start

If you want to quickly start testing Music Server you can use the Docker Compose file in this repository.
Open the file `docker-compose.yml` and modify the `/change/me` to a local path where music is stored. Save the file and run it with

```
docker compose pull
docker compose -f docker-compose.yml up -d
```

Open the browser of your choice and go to `http://localhost:3001`. The Administrative UI is shown to you.
In the plugins section click on the `Scan` button next to the File System plugin to scan for song, albums and artists.

### Volumio plugin

The Music Server has Rest APIs that you can use to browse content, search by text and stream songs. If you plan to use with Volumio connect to it via SSH and perform the following:

```
cd /data/plugins/music_service
git clone https://github.com/fanciulli/musicserver-volumio-plugin.git musicserver
cd musicserver
npm install
```

Now edit the file `plugins.json` under /data/plugins in order to add the following under the field `music_service`:

```
"musciserver": {
     		"enabled": {
        	"type": "boolean",
        	"value": true
      	}
```

Restart Volumio. In Volumio UI go to Plugins > Music Server and click on `Settings`. The configuration page is shown. Update it based on your current environment:

![Plugin Settings on Volumio](./media/plugin_settings_on_volumio.png)

Restart Volumio. The Browse shall now show a new source.

## Standalone redistributable (no Docker)

This repo can build a self-contained bundle that includes the backend, the admin UI, a Node.js runtime, and MongoDB. End users unzip the archive and double-click one launcher binary — no Node, MongoDB, or Docker required on the target machine.

### Build prerequisites

- Node.js 20 LTS (only needed on the build machine)
- `git`, `curl`, `tar`, `unzip`, `zip`
- macOS builders also need the Xcode command-line tools for `codesign`

### Build for your current platform

```bash
node packaging/build.mjs --target host
```

Outputs:

- `packaging/dist/musicserver-<os>-<arch>/` — the assembled bundle
- `packaging/dist/musicserver-<os>-<arch>.tar.gz` (or `.zip` on Windows targets)

### Build for a specific platform

```bash
node packaging/build.mjs --target macos-arm64
node packaging/build.mjs --target macos-x64
node packaging/build.mjs --target linux-x64
node packaging/build.mjs --target linux-arm64
node packaging/build.mjs --target win-x64
```

Cross-target builds work because the launcher is built by injecting a SEA blob into a downloaded target-platform `node` binary. macOS targets built from a non-macOS host will not be code-signed (Gatekeeper will block them); produce the macOS bundle on a Mac for distribution.

### Useful flags

- `--offline` — skip `git pull` and re-downloads (use cached sources and runtimes)
- `--skip-archive` — leave the unzipped bundle in `packaging/dist/` without producing a `.tar.gz`/`.zip`

### Running the bundle

Unzip the archive, then:

- **macOS / Linux:** `./musicserver`
- **Windows:** double-click `musicserver.exe`

The launcher starts MongoDB on `127.0.0.1:27017`, the backend on `:3000`, and the admin UI on `:3001`, then opens `http://localhost:3001` in the default browser. Data lives under `data/` next to the binary (MongoDB files in `data/mongodb`, all logs in `data/logs`).

Press `Ctrl+C` (or close the console window) to stop everything cleanly.

### Bundle layout

```
musicserver-<os>-<arch>/
  musicserver(.exe)        ← launcher (the only thing users run)
  runtime/                 ← bundled Node.js + mongod
  app/backend/             ← compiled Fastify server
  app/ui/                  ← Next.js standalone build
  data/                    ← created on first run (DB + logs)
  BUILD-INFO.txt           ← target, versions, and source commit SHAs
```

See `docs/superpowers/specs/2026-05-15-packaging-design.md` for the full design rationale, including why this is a per-platform launcher + sibling tree rather than a literal single `.exe`.
