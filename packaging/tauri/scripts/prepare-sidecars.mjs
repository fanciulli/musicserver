#!/usr/bin/env node
// Prepare Tauri sidecars + resources for the Music Server packages.
//
// For each Tauri app (backend / frontend) this script:
//   1. Clones/updates and builds the relevant source repo(s).
//   2. Downloads the target-platform Node.js runtime (both apps) and MongoDB
//      (backend only) and drops them into `src-tauri/binaries/` using the
//      Rust target-triple suffix Tauri expects for sidecars
//      (e.g. `node-x86_64-unknown-linux-gnu`).
//   3. Copies the compiled application code into `src-tauri/resources/` so the
//      Rust launcher can run it with the bundled Node runtime.
//
// Run this BEFORE `npm run tauri build` (it is wired as the `beforeBuildCommand`
// in each tauri.conf.json, but can also be run on its own).
//
// Usage:
//   node packaging/tauri/scripts/prepare-sidecars.mjs --app both   --target host
//   node packaging/tauri/scripts/prepare-sidecars.mjs --app backend  --target linux-x64
//   node packaging/tauri/scripts/prepare-sidecars.mjs --app frontend --target macos-arm64
//   ... --offline      # do not git pull / re-download (reuse caches)
//   ... --skip-build   # reuse the already-built sources in .work (just re-stage)
//
// Targets: host | macos-arm64 | macos-x64 | linux-x64 | linux-arm64 | win-x64

import { execSync, spawnSync } from "node:child_process";
import {
  createWriteStream, existsSync, mkdirSync, readFileSync,
  rmSync, cpSync, chmodSync,
} from "node:fs";
import { dirname, join, resolve, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { arch as hostArch, platform as hostPlatform } from "node:process";
import { pipeline } from "node:stream/promises";
import https from "node:https";

const __dirname = dirname(fileURLToPath(import.meta.url));
const TAURI_DIR = resolve(__dirname, "..");
const WORK_DIR = join(TAURI_DIR, ".work");
const CACHE_DIR = join(TAURI_DIR, ".cache");
const SRC_DIR = join(WORK_DIR, "src");

const versions = JSON.parse(readFileSync(join(TAURI_DIR, "versions.json"), "utf8"));

// target id -> { node fetch params, mongo fetch params, rust triple }
const TARGETS = {
  "macos-arm64": {
    triple: "aarch64-apple-darwin",
    nodePlatform: "darwin", nodeArch: "arm64", nodeExt: "tar.gz", isWin: false,
    mongoBuild: (v) => `mongodb-macos-aarch64-${v}`,
    mongoUrl: (v) => `https://fastdl.mongodb.org/osx/mongodb-macos-arm64-${v}.tgz`,
  },
  "macos-x64": {
    triple: "x86_64-apple-darwin",
    nodePlatform: "darwin", nodeArch: "x64", nodeExt: "tar.gz", isWin: false,
    mongoBuild: (v) => `mongodb-macos-x86_64-${v}`,
    mongoUrl: (v) => `https://fastdl.mongodb.org/osx/mongodb-macos-x86_64-${v}.tgz`,
  },
  "linux-x64": {
    triple: "x86_64-unknown-linux-gnu",
    nodePlatform: "linux", nodeArch: "x64", nodeExt: "tar.xz", isWin: false,
    mongoBuild: (v, d) => `mongodb-linux-x86_64-${d}-${v}`,
    mongoUrl: (v, d) => `https://fastdl.mongodb.org/linux/mongodb-linux-x86_64-${d}-${v}.tgz`,
  },
  "linux-arm64": {
    triple: "aarch64-unknown-linux-gnu",
    nodePlatform: "linux", nodeArch: "arm64", nodeExt: "tar.xz", isWin: false,
    mongoBuild: (v, d) => `mongodb-linux-aarch64-${d}-${v}`,
    mongoUrl: (v, d) => `https://fastdl.mongodb.org/linux/mongodb-linux-aarch64-${d}-${v}.tgz`,
  },
  "win-x64": {
    triple: "x86_64-pc-windows-msvc",
    nodePlatform: "win", nodeArch: "x64", nodeExt: "zip", isWin: true,
    mongoBuild: (v) => `mongodb-win32-x86_64-windows-${v}`,
    mongoUrl: (v) => `https://fastdl.mongodb.org/windows/mongodb-windows-x86_64-${v}.zip`,
  },
};

const args = parseArgs(process.argv.slice(2));
const OFFLINE = !!args.offline;
const SKIP_BUILD = !!args["skip-build"];
const appArg = (args.app ?? "both").toLowerCase();
const APPS = appArg === "both" ? ["backend", "frontend"] : [appArg];
if (!["backend", "frontend", "both"].includes(appArg)) {
  fail(`unknown --app '${appArg}' (expected backend | frontend | both)`);
}

// Target precedence: explicit --target arg > MUSICSERVER_TARGET env > host.
// The env lets CI cross-builds (e.g. x86_64 macOS on an arm64 runner) stage the
// right sidecars while the beforeBuildCommand stays target-agnostic.
const target = resolveTarget(args.target ?? process.env.MUSICSERVER_TARGET ?? "host");

await main();

// ────────────────────────────────────────────────────────────────────────────

async function main() {
  log(`Target: ${target.id} (${target.triple})  apps: ${APPS.join(", ")}`);
  ensureDir(WORK_DIR);
  ensureDir(CACHE_DIR);
  ensureDir(SRC_DIR);

  const needBackend = APPS.includes("backend");
  const needFrontend = APPS.includes("frontend");

  if (needBackend) {
    await syncRepo("backend", versions.repos.backend);
    if (!SKIP_BUILD) buildBackend();
  }
  if (needFrontend) {
    await syncRepo("ui", versions.repos.ui);
    if (!SKIP_BUILD) buildUi();
  }

  const nodeBin = await fetchNode(target);

  if (needBackend) {
    const mongodBin = await fetchMongo(target);
    stageBackend(nodeBin, mongodBin);
  }
  if (needFrontend) {
    stageFrontend(nodeBin);
  }

  log("DONE — sidecars and resources are staged. You can now run `npm run tauri build`.");
}

// ── target resolution ───────────────────────────────────────────────────────

function resolveTarget(name) {
  if (name === "host") {
    const map = {
      "darwin-arm64": "macos-arm64",
      "darwin-x64": "macos-x64",
      "linux-x64": "linux-x64",
      "linux-arm64": "linux-arm64",
      "win32-x64": "win-x64",
    };
    name = map[`${hostPlatform}-${hostArch}`];
    if (!name) fail(`unsupported host: ${hostPlatform}-${hostArch}`);
  }
  const t = TARGETS[name];
  if (!t) fail(`unknown target: ${name}`);
  return { id: name, ...t };
}

// ── repo sync + builds ──────────────────────────────────────────────────────

async function syncRepo(name, cfg) {
  const dir = join(SRC_DIR, name);
  if (!existsSync(dir)) {
    log(`cloning ${name} (${cfg.url}@${cfg.ref})`);
    run("git", ["clone", "--depth", "1", "--branch", cfg.ref, cfg.url, dir]);
  } else if (!OFFLINE) {
    log(`updating ${name} -> ${cfg.ref}`);
    // Fetch the configured ref and hard-reset to it. We reset to FETCH_HEAD
    // rather than origin/<ref>: the initial clone is single-branch (--depth 1
    // --branch implies --single-branch), so the remote-tracking ref for a
    // different ref (e.g. after changing versions.json) does not exist.
    run("git", ["fetch", "--depth", "1", "origin", cfg.ref], { cwd: dir });
    run("git", ["reset", "--hard", "FETCH_HEAD"], { cwd: dir });
  } else {
    log(`offline: leaving ${name} as-is`);
  }
}

function npmInstall(dir, opts = {}) {
  const hasLock = existsSync(join(dir, "package-lock.json")) || existsSync(join(dir, "npm-shrinkwrap.json"));
  const cmd = hasLock ? ["ci"] : ["install", "--no-audit", "--no-fund"];
  if (opts.prod) cmd.push("--omit=dev");
  if (opts.ignoreScripts) cmd.push("--ignore-scripts");
  run("npm", cmd, { cwd: dir });
}

function buildBackend() {
  const dir = join(SRC_DIR, "backend");
  log("backend: installing deps");
  npmInstall(dir);
  log("backend: npm run build");
  run("npm", ["run", "build"], { cwd: dir });
}

function buildUi() {
  const dir = join(SRC_DIR, "ui");
  log("ui: installing deps");
  npmInstall(dir);
  log("ui: npm run build (next build)");
  run("npm", ["run", "build"], { cwd: dir, env: { ...process.env, NEXT_TELEMETRY_DISABLED: "1" } });
  // Compile the custom TLS server (server.ts -> server.js + src/lib/tls/*.js).
  // This is the entry point we run in the bundle (it supports HTTPS and
  // self-signed cert generation), mirroring the admin-ui Dockerfile.
  log("ui: compiling custom server (tsc -p tsconfig.server.json)");
  run("npx", ["tsc", "-p", "tsconfig.server.json"], { cwd: dir });
}

// ── runtime fetch ───────────────────────────────────────────────────────────

async function fetchNode(t) {
  const v = versions.node;
  const fname = `node-v${v}-${t.nodePlatform}-${t.nodeArch}.${t.nodeExt}`;
  const url = `https://nodejs.org/dist/v${v}/${fname}`;
  const archivePath = join(CACHE_DIR, fname);
  const extractDir = join(CACHE_DIR, `node-v${v}-${t.nodePlatform}-${t.nodeArch}`);
  if (!existsSync(extractDir)) {
    if (!existsSync(archivePath)) await download(url, archivePath);
    log(`extracting node -> ${extractDir}`);
    extract(archivePath, CACHE_DIR);
  }
  const nodeExe = t.isWin ? join(extractDir, "node.exe") : join(extractDir, "bin", "node");
  if (!existsSync(nodeExe)) fail(`node binary not found at ${nodeExe}`);
  return nodeExe;
}

async function fetchMongo(t) {
  const cfg = versions.mongodb[t.id];
  if (!cfg) fail(`no mongodb version configured for ${t.id}`);
  const url = t.mongoUrl(cfg.version, cfg.distro);
  const fname = basename(url);
  const archivePath = join(CACHE_DIR, fname);
  const expectedDir = join(CACHE_DIR, t.mongoBuild(cfg.version, cfg.distro));
  if (!existsSync(expectedDir)) {
    if (!existsSync(archivePath)) await download(url, archivePath);
    log(`extracting mongodb -> ${expectedDir}`);
    extract(archivePath, CACHE_DIR);
  }
  const mongod = join(expectedDir, "bin", t.isWin ? "mongod.exe" : "mongod");
  if (!existsSync(mongod)) fail(`mongod binary not found at ${mongod}`);
  return mongod;
}

// ── staging ─────────────────────────────────────────────────────────────────

// Tauri resolves sidecars by `<name>-<target-triple>(.exe)`. We name the
// sidecars `node` and `mongod`, referenced in tauri.conf.json as
// `binaries/node` and `binaries/mongod`.
function sidecarName(stem) {
  return `${stem}-${target.triple}${target.isWin ? ".exe" : ""}`;
}

function placeBinary(src, appDir, stem) {
  const binDir = join(appDir, "src-tauri", "binaries");
  ensureDir(binDir);
  const dest = join(binDir, sidecarName(stem));
  cpSync(src, dest);
  if (!target.isWin) chmodSync(dest, 0o755);
  log(`sidecar: ${dest}`);
}

function freshResources(appDir) {
  const resDir = join(appDir, "src-tauri", "resources");
  if (existsSync(resDir)) rmSync(resDir, { recursive: true, force: true });
  ensureDir(resDir);
  return resDir;
}

function stageBackend(nodeBin, mongodBin) {
  const appDir = join(TAURI_DIR, "backend");
  placeBinary(nodeBin, appDir, "node");
  placeBinary(mongodBin, appDir, "mongod");

  const resDir = freshResources(appDir);
  const out = join(resDir, "backend");
  ensureDir(out);
  const backendSrc = join(SRC_DIR, "backend");
  cpSync(join(backendSrc, "dist"), join(out, "dist"), { recursive: true });
  cpSync(join(backendSrc, "package.json"), join(out, "package.json"));
  const lock = join(backendSrc, "package-lock.json");
  if (existsSync(lock)) cpSync(lock, join(out, "package-lock.json"));
  log("backend: installing prod deps into resources");
  npmInstall(out, { prod: true, ignoreScripts: true });
}

// Stage the admin UI to run via its custom `server.ts` (HTTPS-capable). The
// custom server uses `next({ dev: false })`, which requires a COMPLETE `next
// build` output in `.next` plus the prod node_modules — i.e. exactly what
// `npm start` (`node server.js`) uses. We therefore stage the full `.next`
// build (not the trimmed `.next/standalone` tree, which is only valid for
// Next's own standalone server and lacks the production build markers).
function stageFrontend(nodeBin) {
  const appDir = join(TAURI_DIR, "frontend");
  placeBinary(nodeBin, appDir, "node");

  const uiSrc = join(SRC_DIR, "ui");
  const nextDir = join(uiSrc, ".next");
  if (!existsSync(nextDir)) {
    fail(`Next.js build output not found at ${nextDir}. Run 'next build' first.`);
  }
  const customServer = join(uiSrc, "server.js");
  if (!existsSync(customServer)) {
    fail(`Compiled custom server not found at ${customServer}. Ensure 'tsc -p tsconfig.server.json' ran.`);
  }

  const resDir = freshResources(appDir);
  const out = join(resDir, "ui");
  ensureDir(out);

  // 1. prod deps (provides next, react, selfsigned, …)
  cpSync(join(uiSrc, "package.json"), join(out, "package.json"));
  const lock = join(uiSrc, "package-lock.json");
  if (existsSync(lock)) cpSync(lock, join(out, "package-lock.json"));
  log("ui: installing prod deps into resources");
  npmInstall(out, { prod: true, ignoreScripts: true });

  // 2. full Next.js production build (.next), excluding the build cache and the
  //    standalone subtree (neither is needed to run `next({ dev: false })`).
  log("ui: copying .next production build");
  cpSync(nextDir, join(out, ".next"), {
    recursive: true,
    filter: (src) => {
      const rel = src.slice(nextDir.length).replace(/^[\\/]/, "");
      const top = rel.split(/[\\/]/)[0];
      return top !== "cache" && top !== "standalone";
    },
  });

  // 3. public assets
  const publicSrc = join(uiSrc, "public");
  if (existsSync(publicSrc)) cpSync(publicSrc, join(out, "public"), { recursive: true });

  // 4. custom TLS server (compiled from server.ts) + its TLS helper
  cpSync(customServer, join(out, "server.js"));
  const tlsSrc = join(uiSrc, "src", "lib", "tls");
  if (existsSync(tlsSrc)) cpSync(tlsSrc, join(out, "src", "lib", "tls"), { recursive: true });
}

// ── util ────────────────────────────────────────────────────────────────────

function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (!a.startsWith("--")) continue;
    const key = a.slice(2);
    const next = argv[i + 1];
    if (next && !next.startsWith("--")) { out[key] = next; i++; }
    else out[key] = true;
  }
  return out;
}

function ensureDir(p) { if (!existsSync(p)) mkdirSync(p, { recursive: true }); }
function log(msg) { process.stdout.write(`[prepare] ${msg}\n`); }
function fail(msg) { process.stderr.write(`[prepare] ERROR: ${msg}\n`); process.exit(1); }

function run(cmd, args, opts = {}) {
  // On Windows, `npm`/`npx` are `.cmd` scripts; Node refuses to spawn those
  // without a shell (since the CVE-2024-27980 fix), so run them through cmd.exe.
  // Their args here have no spaces, so shell quoting is not a concern.
  const useShell = process.platform === "win32" && (cmd === "npm" || cmd === "npx");
  const r = spawnSync(cmd, args, { stdio: "inherit", shell: useShell, ...opts });
  if (r.status !== 0) fail(`command failed: ${cmd} ${args.join(" ")} (exit ${r.status})`);
}

async function download(url, dest) {
  log(`downloading ${url}`);
  await new Promise((resolveP, rejectP) => {
    const req = https.get(url, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume();
        download(res.headers.location, dest).then(resolveP, rejectP);
        return;
      }
      if (res.statusCode !== 200) { rejectP(new Error(`HTTP ${res.statusCode} for ${url}`)); return; }
      pipeline(res, createWriteStream(dest)).then(resolveP, rejectP);
    });
    req.on("error", rejectP);
  });
}

function extract(archivePath, outDir) {
  if (archivePath.endsWith(".zip")) {
    if (hostPlatform === "win32") {
      run("powershell", ["-Command", `Expand-Archive -Force -Path '${archivePath}' -DestinationPath '${outDir}'`]);
    } else {
      run("unzip", ["-q", "-o", archivePath, "-d", outDir]);
    }
  } else if (archivePath.endsWith(".tar.gz") || archivePath.endsWith(".tgz")) {
    run("tar", ["-xzf", archivePath, "-C", outDir]);
  } else if (archivePath.endsWith(".tar.xz")) {
    run("tar", ["-xJf", archivePath, "-C", outDir]);
  } else {
    fail(`unknown archive format: ${archivePath}`);
  }
}
