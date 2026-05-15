// Music Server launcher (CommonJS — required by Node.js SEA).
// Spawns mongod, backend, UI; opens the user's browser; manages shutdown.
// Compiled into a Single Executable Application by packaging/build.mjs.

"use strict";

const { spawn } = require("node:child_process");
const { existsSync, mkdirSync, openSync, appendFileSync } = require("node:fs");
const { dirname, join, resolve } = require("node:path");
const { platform } = require("node:process");
const net = require("node:net");

const IS_WIN = platform === "win32";
const EXE_EXT = IS_WIN ? ".exe" : "";

const BUNDLE_ROOT = resolve(dirname(process.execPath));
const RUNTIME_DIR = join(BUNDLE_ROOT, "runtime");
const APP_DIR = join(BUNDLE_ROOT, "app");
const DATA_DIR = join(BUNDLE_ROOT, "data");
const LOG_DIR = join(DATA_DIR, "logs");
const MONGO_DATA_DIR = join(DATA_DIR, "mongodb");

const NODE_BIN = join(RUNTIME_DIR, "node" + EXE_EXT);
const MONGOD_BIN = join(RUNTIME_DIR, "mongod" + EXE_EXT);
const BACKEND_DIR = join(APP_DIR, "backend");
const UI_DIR = join(APP_DIR, "ui");

const PORTS = { mongo: 27017, backend: 3000, ui: 3001 };
const HOST = "127.0.0.1";

const children = [];
let shuttingDown = false;

function logLine(msg) {
  const line = "[" + new Date().toISOString() + "] " + msg + "\n";
  process.stdout.write(line);
  try {
    appendFileSync(join(LOG_DIR, "launcher.log"), line);
  } catch (_) {
    // log dir may not exist yet on the very first call
  }
}

function ensureDirs() {
  for (const d of [DATA_DIR, LOG_DIR, MONGO_DATA_DIR]) {
    if (!existsSync(d)) mkdirSync(d, { recursive: true });
  }
}

function assertLayout() {
  const required = [
    NODE_BIN,
    MONGOD_BIN,
    join(BACKEND_DIR, "dist", "index.js"),
    join(UI_DIR, "server.js"),
  ];
  const missing = required.filter(function (p) { return !existsSync(p); });
  if (missing.length) {
    logLine("FATAL: required files missing:\n  - " + missing.join("\n  - "));
    process.exit(2);
  }
}

function waitForPort(port, timeoutMs) {
  if (timeoutMs == null) timeoutMs = 30000;
  const deadline = Date.now() + timeoutMs;
  return new Promise(function (resolveP, rejectP) {
    function tryOnce() {
      const sock = net.createConnection({ host: HOST, port: port });
      sock.once("connect", function () {
        sock.destroy();
        resolveP();
      });
      sock.once("error", function () {
        sock.destroy();
        if (Date.now() > deadline) {
          rejectP(new Error("timeout waiting for " + HOST + ":" + port));
        } else {
          setTimeout(tryOnce, 250);
        }
      });
    }
    tryOnce();
  });
}

function checkPortFree(port) {
  return new Promise(function (resolveP, rejectP) {
    const srv = net.createServer();
    srv.once("error", function (err) { rejectP(err); });
    srv.once("listening", function () { srv.close(function () { resolveP(); }); });
    srv.listen(port, HOST);
  });
}

function spawnChild(name, bin, args, opts) {
  if (!opts) opts = {};
  const logPath = join(LOG_DIR, name + ".log");
  const out = openSync(logPath, "a");
  const err = openSync(logPath, "a");
  const proc = spawn(bin, args, Object.assign({
    stdio: ["ignore", out, err],
    detached: false,
  }, opts));
  children.push({ name: name, proc: proc });
  proc.on("exit", function (code, signal) {
    if (!shuttingDown) {
      logLine("child '" + name + "' exited unexpectedly (code=" + code + " signal=" + signal + "); shutting down");
      shutdown(1);
    }
  });
  logLine("spawned " + name + " (pid=" + proc.pid + ") -> " + logPath);
  return proc;
}

function openBrowser(url) {
  try {
    if (IS_WIN) {
      spawn("cmd", ["/c", "start", "", url], { detached: true, stdio: "ignore" }).unref();
    } else if (platform === "darwin") {
      spawn("open", [url], { detached: true, stdio: "ignore" }).unref();
    } else {
      spawn("xdg-open", [url], { detached: true, stdio: "ignore" }).unref();
    }
  } catch (e) {
    logLine("could not open browser: " + e.message);
  }
}

async function shutdown(exitCode) {
  if (exitCode == null) exitCode = 0;
  if (shuttingDown) return;
  shuttingDown = true;
  logLine("shutting down...");
  const reversed = children.slice().reverse();
  for (const entry of reversed) {
    if (entry.proc.exitCode !== null) continue;
    try { entry.proc.kill("SIGTERM"); logLine("SIGTERM -> " + entry.name); } catch (_) {}
  }
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline) {
    const alive = children.filter(function (e) { return e.proc.exitCode === null; });
    if (alive.length === 0) break;
    await new Promise(function (r) { setTimeout(r, 250); });
  }
  for (const entry of children) {
    if (entry.proc.exitCode === null) {
      try { entry.proc.kill("SIGKILL"); logLine("SIGKILL -> " + entry.name); } catch (_) {}
    }
  }
  process.exit(exitCode);
}

async function main() {
  ensureDirs();
  logLine("Music Server launcher starting in " + BUNDLE_ROOT);
  assertLayout();

  for (const name of Object.keys(PORTS)) {
    const port = PORTS[name];
    try {
      await checkPortFree(port);
    } catch (_) {
      logLine("FATAL: port " + port + " (" + name + ") already in use on " + HOST);
      process.exit(3);
    }
  }

  logLine("starting MongoDB...");
  spawnChild("mongod", MONGOD_BIN, [
    "--dbpath", MONGO_DATA_DIR,
    "--port", String(PORTS.mongo),
    "--bind_ip", HOST,
    "--logpath", join(LOG_DIR, "mongod.log"),
    "--logappend",
  ]);
  await waitForPort(PORTS.mongo);
  logLine("MongoDB ready");

  logLine("starting backend...");
  // The backend resolves runtime paths (types/db, plugins, logs) relative to
  // process.cwd(), so we must invoke it from its dist directory — matching
  // the upstream `cd dist; node index.js` start command.
  spawnChild("backend", NODE_BIN, ["index.js"], {
    cwd: join(BACKEND_DIR, "dist"),
    env: Object.assign({}, process.env, { MONGO_URI: "mongodb://" + HOST + ":" + PORTS.mongo }),
  });
  await waitForPort(PORTS.backend);
  logLine("backend ready");

  logLine("starting UI...");
  spawnChild("ui", NODE_BIN, [join(UI_DIR, "server.js")], {
    cwd: UI_DIR,
    env: Object.assign({}, process.env, {
      PORT: String(PORTS.ui),
      HOSTNAME: HOST,
      MUSICSERVER_API_BASE_URL: "http://" + HOST + ":" + PORTS.backend,
    }),
  });
  await waitForPort(PORTS.ui);
  logLine("UI ready");

  const url = "http://localhost:" + PORTS.ui;
  logLine("opening browser at " + url);
  openBrowser(url);

  logLine("Music Server is running. Press Ctrl+C to stop.");
}

process.on("SIGINT", function () { shutdown(0); });
process.on("SIGTERM", function () { shutdown(0); });
if (IS_WIN) process.on("SIGHUP", function () { shutdown(0); });

main().catch(function (err) {
  logLine("FATAL: " + (err.stack || err.message || err));
  shutdown(1);
});
