#!/usr/bin/env node
// Music Server packaging orchestrator.
// Produces a per-platform redistributable bundle under packaging/dist/.
//
// Usage:
//   node packaging/build.mjs --target host
//   node packaging/build.mjs --target macos-arm64
//   node packaging/build.mjs --target linux-x64
//   node packaging/build.mjs --target win-x64
//   node packaging/build.mjs --skip-archive            # leave dist tree unzipped
//   node packaging/build.mjs --offline                 # do not git pull / re-download
//
// Targets:
//   host | macos-arm64 | macos-x64 | linux-x64 | linux-arm64 | win-x64

import { execSync, spawnSync } from "node:child_process";
import { createWriteStream, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync, cpSync, chmodSync, statSync, readdirSync } from "node:fs";
import { dirname, join, resolve, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { arch as hostArch, platform as hostPlatform } from "node:process";
import { pipeline } from "node:stream/promises";
import { Readable } from "node:stream";
import https from "node:https";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, "..");
const PKG_DIR = __dirname;
const WORK_DIR = join(PKG_DIR, ".work");
const CACHE_DIR = join(PKG_DIR, ".cache");
const SRC_DIR = join(WORK_DIR, "src");
const STAGE_DIR = join(WORK_DIR, "stage");
const DIST_DIR = join(PKG_DIR, "dist");
const LAUNCHER_DIR = join(PKG_DIR, "launcher");

const versions = JSON.parse(readFileSync(join(PKG_DIR, "versions.json"), "utf8"));

const TARGETS = {
  "macos-arm64": {
    nodePlatform: "darwin",
    nodeArch: "arm64",
    nodeExt: "tar.gz",
    isWin: false,
    archiveExt: "tar.gz",
    mongoBuild: (v) => `mongodb-macos-aarch64-${v}`,
    mongoUrl: (v) => `https://fastdl.mongodb.org/osx/mongodb-macos-arm64-${v}.tgz`,
  },
  "macos-x64": {
    nodePlatform: "darwin",
    nodeArch: "x64",
    nodeExt: "tar.gz",
    isWin: false,
    archiveExt: "tar.gz",
    mongoBuild: (v) => `mongodb-macos-x86_64-${v}`,
    mongoUrl: (v) => `https://fastdl.mongodb.org/osx/mongodb-macos-x86_64-${v}.tgz`,
  },
  "linux-x64": {
    nodePlatform: "linux",
    nodeArch: "x64",
    nodeExt: "tar.xz",
    isWin: false,
    archiveExt: "tar.gz",
    mongoBuild: (v, d) => `mongodb-linux-x86_64-${d}-${v}`,
    mongoUrl: (v, d) => `https://fastdl.mongodb.org/linux/mongodb-linux-x86_64-${d}-${v}.tgz`,
  },
  "linux-arm64": {
    nodePlatform: "linux",
    nodeArch: "arm64",
    nodeExt: "tar.xz",
    isWin: false,
    archiveExt: "tar.gz",
    mongoBuild: (v, d) => `mongodb-linux-aarch64-${d}-${v}`,
    mongoUrl: (v, d) => `https://fastdl.mongodb.org/linux/mongodb-linux-aarch64-${d}-${v}.tgz`,
  },
  "win-x64": {
    nodePlatform: "win",
    nodeArch: "x64",
    nodeExt: "zip",
    isWin: true,
    archiveExt: "zip",
    mongoBuild: (v) => `mongodb-win32-x86_64-windows-${v}`,
    mongoUrl: (v) => `https://fastdl.mongodb.org/windows/mongodb-windows-x86_64-${v}.zip`,
  },
};

const args = parseArgs(process.argv.slice(2));
const SKIP_ARCHIVE = !!args["skip-archive"];
const OFFLINE = !!args.offline;
let target;

await main();

// ────────────────────────────────────────────────────────────────────────────

async function main() {
  target = resolveTarget(args.target ?? "host");
  log(`Target: ${target.id}  (host=${hostPlatform}-${hostArch})`);
  ensureDir(WORK_DIR);
  ensureDir(CACHE_DIR);
  ensureDir(SRC_DIR);
  ensureDir(STAGE_DIR);
  ensureDir(DIST_DIR);

  await syncRepo("backend", versions.repos.backend);
  await syncRepo("ui", versions.repos.ui);

  buildBackend();
  buildUi();

  const nodePath = await fetchNode(target);
  const mongoDir = await fetchMongo(target);

  const bundleDir = join(STAGE_DIR, `musicserver-${target.id}`);
  if (existsSync(bundleDir)) rmSync(bundleDir, { recursive: true, force: true });
  ensureDir(bundleDir);

  assembleBundle(bundleDir, nodePath, mongoDir);

  await buildLauncher(target, nodePath, bundleDir);

  writeBuildInfo(bundleDir);

  const finalDir = join(DIST_DIR, `musicserver-${target.id}`);
  if (existsSync(finalDir)) rmSync(finalDir, { recursive: true, force: true });
  cpSync(bundleDir, finalDir, { recursive: true });

  if (!SKIP_ARCHIVE) {
    archive(finalDir);
  } else {
    log(`skipping archive (--skip-archive); bundle at ${finalDir}`);
  }

  log("DONE");
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
    const key = `${hostPlatform}-${hostArch}`;
    name = map[key];
    if (!name) throw new Error(`unsupported host: ${key}`);
  }
  const t = TARGETS[name];
  if (!t) throw new Error(`unknown target: ${name}`);
  return { id: name, ...t };
}

// ── repo sync ───────────────────────────────────────────────────────────────

async function syncRepo(name, cfg) {
  const dir = join(SRC_DIR, name);
  if (!existsSync(dir)) {
    log(`cloning ${name} (${cfg.url}@${cfg.ref})`);
    run("git", ["clone", "--depth", "1", "--branch", cfg.ref, cfg.url, dir]);
  } else if (!OFFLINE) {
    log(`updating ${name}`);
    run("git", ["fetch", "--depth", "1", "origin", cfg.ref], { cwd: dir });
    run("git", ["reset", "--hard", `origin/${cfg.ref}`], { cwd: dir });
  } else {
    log(`offline: leaving ${name} as-is`);
  }
}

// ── builds ──────────────────────────────────────────────────────────────────

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
  log("ui: npm run build");
  run("npm", ["run", "build"], { cwd: dir, env: { ...process.env, NEXT_TELEMETRY_DISABLED: "1" } });
}

// ── runtime fetch ───────────────────────────────────────────────────────────

async function fetchNode(target) {
  const v = versions.node;
  const fname = `node-v${v}-${target.nodePlatform}-${target.nodeArch}.${target.nodeExt}`;
  const url = `https://nodejs.org/dist/v${v}/${fname}`;
  const archivePath = join(CACHE_DIR, fname);
  const extractDir = join(CACHE_DIR, `node-v${v}-${target.nodePlatform}-${target.nodeArch}`);
  if (!existsSync(extractDir)) {
    if (!existsSync(archivePath)) await download(url, archivePath);
    log(`extracting node -> ${extractDir}`);
    extract(archivePath, CACHE_DIR);
  }
  const nodeExe = target.isWin ? join(extractDir, "node.exe") : join(extractDir, "bin", "node");
  if (!existsSync(nodeExe)) throw new Error(`node binary not found at ${nodeExe}`);
  return nodeExe;
}

async function fetchMongo(target) {
  const cfg = versions.mongodb[target.id];
  if (!cfg) throw new Error(`no mongodb version configured for ${target.id}`);
  const url = target.mongoUrl(cfg.version, cfg.distro);
  const fname = basename(url);
  const archivePath = join(CACHE_DIR, fname);
  const expectedDir = join(CACHE_DIR, target.mongoBuild(cfg.version, cfg.distro));
  if (!existsSync(expectedDir)) {
    if (!existsSync(archivePath)) await download(url, archivePath);
    log(`extracting mongodb -> ${expectedDir}`);
    extract(archivePath, CACHE_DIR);
  }
  if (!existsSync(expectedDir)) throw new Error(`mongodb extract dir missing: ${expectedDir}`);
  return expectedDir;
}

// ── bundle assembly ─────────────────────────────────────────────────────────

function assembleBundle(bundleDir, nodePath, mongoDir) {
  const runtimeDir = join(bundleDir, "runtime");
  const appDir = join(bundleDir, "app");
  ensureDir(runtimeDir);
  ensureDir(appDir);

  // 1. node runtime
  const nodeOut = join(runtimeDir, target.isWin ? "node.exe" : "node");
  cpSync(nodePath, nodeOut);
  if (!target.isWin) chmodSync(nodeOut, 0o755);

  // 2. mongod
  const mongoBin = join(mongoDir, "bin");
  const mongodSrc = join(mongoBin, target.isWin ? "mongod.exe" : "mongod");
  const mongodOut = join(runtimeDir, target.isWin ? "mongod.exe" : "mongod");
  cpSync(mongodSrc, mongodOut);
  if (!target.isWin) chmodSync(mongodOut, 0o755);
  // copy mongod runtime deps on Linux/macOS if any sit alongside (e.g. mongod's libs).
  // The community tarballs ship a single static-ish binary, no extras needed.
  // On Windows, mongod.exe is also self-contained.

  // 3. backend (dist + prod node_modules)
  const backendOut = join(appDir, "backend");
  ensureDir(backendOut);
  cpSync(join(SRC_DIR, "backend", "dist"), join(backendOut, "dist"), { recursive: true });
  cpSync(join(SRC_DIR, "backend", "package.json"), join(backendOut, "package.json"));
  const lockSrc = join(SRC_DIR, "backend", "package-lock.json");
  if (existsSync(lockSrc)) cpSync(lockSrc, join(backendOut, "package-lock.json"));
  log("backend: installing prod deps into bundle");
  npmInstall(backendOut, { prod: true, ignoreScripts: true });

  // 4. UI (Next.js standalone)
  const uiSrc = join(SRC_DIR, "ui");
  const uiOut = join(appDir, "ui");
  ensureDir(uiOut);
  const standaloneDir = join(uiSrc, ".next", "standalone");
  if (!existsSync(standaloneDir)) {
    throw new Error(`Next.js standalone output not found at ${standaloneDir}. Ensure next.config has output: "standalone".`);
  }
  cpSync(standaloneDir, uiOut, { recursive: true });
  // Next.js does not auto-copy static + public into standalone; do it now.
  const staticSrc = join(uiSrc, ".next", "static");
  if (existsSync(staticSrc)) {
    cpSync(staticSrc, join(uiOut, ".next", "static"), { recursive: true });
  }
  const publicSrc = join(uiSrc, "public");
  if (existsSync(publicSrc)) {
    cpSync(publicSrc, join(uiOut, "public"), { recursive: true });
  }
}

// ── launcher build (Node SEA) ───────────────────────────────────────────────

async function buildLauncher(target, nodePath, bundleDir) {
  log("building launcher (SEA)");
  const launcherWorkDir = join(WORK_DIR, "launcher");
  if (existsSync(launcherWorkDir)) rmSync(launcherWorkDir, { recursive: true, force: true });
  ensureDir(launcherWorkDir);
  cpSync(join(LAUNCHER_DIR, "launcher.cjs"), join(launcherWorkDir, "launcher.cjs"));
  cpSync(join(LAUNCHER_DIR, "sea-config.json"), join(launcherWorkDir, "sea-config.json"));

  // 1. Generate blob using HOST node (process.execPath). The blob format is
  //    portable across platforms as long as the major version matches.
  run(process.execPath, ["--experimental-sea-config", "sea-config.json"], { cwd: launcherWorkDir });

  // 2. Copy target node binary to become the launcher.
  const launcherName = target.isWin ? "musicserver.exe" : "musicserver";
  const launcherOut = join(bundleDir, launcherName);
  cpSync(nodePath, launcherOut);
  if (!target.isWin) chmodSync(launcherOut, 0o755);

  // 3. Remove macOS code signature before injecting (otherwise mach-o is invalid).
  if (target.nodePlatform === "darwin" && hostPlatform === "darwin") {
    try {
      run("codesign", ["--remove-signature", launcherOut]);
    } catch (e) {
      log(`warn: codesign --remove-signature failed: ${e.message}`);
    }
  }

  // 4. Inject blob via postject (installed transiently via npx).
  const postjectArgs = [
    "--yes",
    "postject@1.0.0-alpha.6",
    launcherOut,
    "NODE_SEA_BLOB",
    join(launcherWorkDir, "launcher.blob"),
    "--sentinel-fuse",
    "NODE_SEA_FUSE_fce680ab2cc467b6e072b8b5df1996b2",
  ];
  if (target.nodePlatform === "darwin") postjectArgs.push("--macho-segment-name", "NODE_SEA");
  run("npx", postjectArgs);

  // 5. Re-sign on macOS host so the launcher passes the system's signature check.
  if (target.nodePlatform === "darwin" && hostPlatform === "darwin") {
    try {
      run("codesign", ["--sign", "-", launcherOut]);
    } catch (e) {
      log(`warn: ad-hoc codesign failed: ${e.message}`);
    }
  }

  if (target.nodePlatform === "darwin" && hostPlatform !== "darwin") {
    log("warn: building macOS launcher on a non-macOS host; Gatekeeper will reject without signing on a Mac");
  }
}

// ── build-info & archive ────────────────────────────────────────────────────

function writeBuildInfo(bundleDir) {
  const backendSha = git(["rev-parse", "HEAD"], join(SRC_DIR, "backend"));
  const uiSha = git(["rev-parse", "HEAD"], join(SRC_DIR, "ui"));
  const info = [
    `target:        ${target.id}`,
    `built:         ${new Date().toISOString()}`,
    `node:          v${versions.node}`,
    `mongodb:       v${versions.mongodb[target.id].version}`,
    `backend ref:   ${versions.repos.backend.ref} (${backendSha})`,
    `ui ref:        ${versions.repos.ui.ref} (${uiSha})`,
    "",
  ].join("\n");
  writeFileSync(join(bundleDir, "BUILD-INFO.txt"), info);
}

function archive(dir) {
  const name = basename(dir);
  if (target.archiveExt === "zip") {
    const out = join(DIST_DIR, `${name}.zip`);
    if (existsSync(out)) rmSync(out);
    if (hostPlatform === "win32") {
      run("powershell", ["-Command", `Compress-Archive -Path '${dir}/*' -DestinationPath '${out}'`]);
    } else {
      run("zip", ["-r", out, name], { cwd: DIST_DIR });
    }
    log(`archive: ${out}`);
  } else {
    const out = join(DIST_DIR, `${name}.tar.gz`);
    if (existsSync(out)) rmSync(out);
    run("tar", ["-czf", out, "-C", DIST_DIR, name]);
    log(`archive: ${out}`);
  }
}

// ── util ────────────────────────────────────────────────────────────────────

function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (!a.startsWith("--")) continue;
    const key = a.slice(2);
    const next = argv[i + 1];
    if (next && !next.startsWith("--")) {
      out[key] = next;
      i++;
    } else {
      out[key] = true;
    }
  }
  return out;
}

function ensureDir(p) {
  if (!existsSync(p)) mkdirSync(p, { recursive: true });
}

function log(msg) {
  process.stdout.write(`[build] ${msg}\n`);
}

function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { stdio: "inherit", shell: false, ...opts });
  if (r.status !== 0) {
    throw new Error(`command failed: ${cmd} ${args.join(" ")} (exit ${r.status})`);
  }
}

function git(args, cwd) {
  return execSync(`git ${args.join(" ")}`, { cwd, encoding: "utf8" }).trim();
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
      if (res.statusCode !== 200) {
        rejectP(new Error(`HTTP ${res.statusCode} for ${url}`));
        return;
      }
      const file = createWriteStream(dest);
      pipeline(res, file).then(resolveP, rejectP);
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
    throw new Error(`unknown archive format: ${archivePath}`);
  }
}
