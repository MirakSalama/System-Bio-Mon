// System Bio Mon's legacy local browser dashboard.
"use strict";

const http = require("http");
const fs = require("fs");
const path = require("path");
const os = require("os");
const { spawn } = require("child_process");
const { WebSocketServer } = require("ws");

const PORT = 4477;
const ROOT = __dirname;
const CONFIG_PATH = path.join(ROOT, "config.json");
const AMD_ROOT = process.env.ADLX_SERVER_ROOT || path.join(os.homedir(), "ADLX", "MCPs", "Server");
const AMD_PY = process.env.ADLX_PYTHON || path.join(AMD_ROOT, ".venv", "Scripts", "python.exe");
const AMD_BRIDGE = path.join(ROOT, "bridge", "amd_bridge.py");

// Keep settings from older installations. Fresh checkouts start with the shared defaults.
const initialConfig = fs.existsSync(CONFIG_PATH) ? CONFIG_PATH : path.join(ROOT, "config.default.json");
let config = JSON.parse(fs.readFileSync(initialConfig, "utf8"));
// Older settings used "fan" for both AMD RPM and NVIDIA duty cycle.
for (const [id, metrics] of Object.entries(config.assignments.perGpu || {})) {
  if (id.startsWith("nv-")) config.assignments.perGpu[id] = [...new Set(metrics.map(key => key === "fan" ? "fanPercent" : key))];
}
if (config.assignments.default.includes("fan") && !config.assignments.default.includes("fanPercent")) {
  config.assignments.default.push("fanPercent");
}
for (const rule of config.alerts || []) {
  if (rule.gpu.startsWith("nv-") && rule.metric === "fan") rule.metric = "fanPercent";
  if (rule.gpu === "sys-0" && ["cpu", "cores"].includes(rule.metric)) rule.gpu = "cpu-0";
}
const perDevice = config.assignments.perGpu || {};
if (Array.isArray(perDevice["sys-0"])) {
  const moved = perDevice["sys-0"].filter(k => k === "cpu" || k === "cores");
  perDevice["sys-0"] = perDevice["sys-0"].filter(k => !moved.includes(k));
  if (perDevice["cpu-0"]) perDevice["cpu-0"] = [...new Set([...perDevice["cpu-0"], ...moved])];
  else if (moved.length === 1) perDevice["cpu-0"] = [...moved, "cpuReportedClock", "busiestThread"];
}

/* ---------------------------------- inventory (live) --------------------------------- */
const inventory = new Map(); // id -> { id, vendor, name }
const latest = new Map();    // id -> { name, metrics, system, state }
const observed = new Map();  // id -> Set of metric keys ever reported

function noteMetrics(id, metrics) {
  let s = observed.get(id);
  if (!s) { s = new Set(); observed.set(id, s); }
  for (const k in metrics) if (Number.isFinite(metrics[k])) s.add(k);
}

/* --------------------------------- NVIDIA adapter ------------------------------------ */
class NvidiaAdapter {
  constructor() {
    this.vendor = "nvidia";
    this.fields = "index,name,temperature.gpu,utilization.gpu,memory.used,memory.total,power.draw,clocks.current.graphics,clocks.current.memory,fan.speed";
    this.startStream();
  }
  startStream() {
    this.mode = "stream";
    try {
      this.proc = spawn("nvidia-smi", ["--query-gpu=" + this.fields, "--format=csv,noheader,nounits", "-l", "1"], { windowsHide: true });
      let buf = "";
      this.proc.stdout.on("data", (d) => {
        buf += d;
        let i;
        while ((i = buf.indexOf("\n")) >= 0) {
          this.parseLine(buf.slice(0, i));
          buf = buf.slice(i + 1);
        }
      });
      this.proc.stderr.on("data", () => {});
      this.proc.on("error", (err) => {
        if (err.code === "ENOENT" || err.code === "EACCES") this.mode = "unavailable";
        else this.fallback();
      });
      this.proc.on("exit", () => this.fallback());
    } catch {
      this.fallback();
    }
  }
  fallback() {
    if (this.mode !== "stream") return;
    this.mode = "oneshot";
    this.tick();
  }
  tick() {
    const p = spawn("nvidia-smi", ["--query-gpu=" + this.fields, "--format=csv,noheader,nounits"], { windowsHide: true });
    let buf = "";
    p.stdout.on("data", (d) => (buf += d));
    p.on("error", (err) => {
      if (err.code === "ENOENT" || err.code === "EACCES") this.mode = "unavailable";
    });
    p.on("close", () => buf.split("\n").filter(Boolean).forEach((l) => this.parseLine(l)));
  }
  startPolling() {
    setInterval(() => this.mode === "oneshot" && this.tick(), 1000);
  }
  num(s) {
    if (s == null) return undefined;
    const v = parseFloat(String(s).trim());
    return Number.isFinite(v) ? v : undefined;
  }
  parseLine(line) {
    const p = line.split(",").map((s) => s.trim());
    if (p.length < 10) return;
    const [index, name, temp, util, memU, memT, power, gfx, mclk, fan] = p;
    const id = `nv-${index}`;
    inventory.set(id, { id, vendor: "nvidia", name });
    const metrics = {
      temperature: this.num(temp),
      usage: this.num(util),
      vramUsed: this.num(memU),
      vramTotal: this.num(memT),
      power: this.num(power),
      clock: this.num(gfx),
      memClock: this.num(mclk),
      fanPercent: this.num(fan),
    };
    latest.set(id, { name, metrics, system: {}, state: "ok" });
    noteMetrics(id, metrics);
  }
}

/* ---------------------------------- AMD adapter --------------------------------------- */
class AmdAdapter {
  constructor() {
    this.vendor = "amd";
    if (process.env.ADLX_PYTHON || fs.existsSync(AMD_PY)) this.startBridge();
    else console.info("[System Bio Mon] Optional AMD Python reader unavailable. Set ADLX_PYTHON or ADLX_SERVER_ROOT to enable it.");
  }
  startBridge() {
    if (this.proc && this.proc.exitCode === null) return;
    try {
      this.proc = spawn(AMD_PY, [AMD_BRIDGE], { windowsHide: true });
      let buf = "";
      this.proc.stdout.on("data", (d) => {
        buf += d;
        let i;
        while ((i = buf.indexOf("\n")) >= 0) {
          const line = buf.slice(0, i);
          buf = buf.slice(i + 1);
          if (!line.trim()) continue;
          try { this.handle(JSON.parse(line)); } catch {}
        }
      });
      this.proc.stderr.on("data", () => {});
      this.proc.on("error", () => console.warn("[System Bio Mon] Could not start the optional AMD Python interpreter."));
      this.proc.on("exit", (code) => {
        for (const [id, g] of inventory) if (g.vendor === "amd") latest.set(id, { state: "offline" });
        if (code === 78) console.warn("[System Bio Mon] The optional AMD reader needs an interpreter with the ADLX Python package.");
        else setTimeout(() => this.startBridge(), 5000);
      });
    } catch {
      setTimeout(() => this.startBridge(), 5000);
    }
  }
  handle(m) {
    if (m.type === "hello") {
      m.gpus.forEach((g) => inventory.set(g.id, { id: g.id, vendor: "amd", name: g.name }));
    } else if (m.type === "frame") {
      inventory.set(m.id, { id: m.id, vendor: "amd", name: m.name });
      latest.set(m.id, { name: m.name, metrics: m.metrics || {}, system: m.system || {}, state: "ok" });
      noteMetrics(m.id, { ...(m.system || {}), ...(m.metrics || {}) });
    }
  }
}

/* ---------------------------------- system adapter ------------------------------------- */
class SystemAdapter {
  constructor() {
    this.prev = os.cpus();
    inventory.set("sys-0", { id: "sys-0", vendor: "system", name: os.hostname() });
    inventory.set("cpu-0", { id: "cpu-0", vendor: "system", name: this.prev[0]?.model || "CPU" });
    setInterval(() => this.tick(), 1000);
  }
  tick() {
    const cpus = os.cpus();
    const memory = {
      ram: Math.round((os.totalmem() - os.freemem()) / 1048576),
      ramTotal: Math.round(os.totalmem() / 1048576),
    };
    latest.set("sys-0", { metrics: memory });
    noteMetrics("sys-0", memory);

    let total = 0, idle = 0;
    const logicalLoads = cpus.map((c, index) => {
      const previous = this.prev[index];
      if (!previous) return null;
      const elapsed = Object.keys(c.times).reduce((sum, k) => sum + c.times[k] - previous.times[k], 0);
      if (elapsed <= 0) return null;
      const idleDelta = c.times.idle - previous.times.idle;
      total += elapsed;
      idle += idleDelta;
      return Math.round(100 * (1 - idleDelta / elapsed));
    });
    if (total > 0) {
      const metrics = {
        cpu: Math.round(1000 * (1 - idle / total)) / 10,
        cores: cpus.length,
        busiestThread: Math.max(...logicalLoads.filter(Number.isFinite)),
      };
      const clocks = cpus.map(c => c.speed).filter(mhz => mhz > 0);
      if (clocks.length) metrics.cpuReportedClock = Math.round(clocks.reduce((a,b) => a+b, 0) / clocks.length);
      latest.set("cpu-0", { metrics, logicalLoads });
      noteMetrics("cpu-0", metrics);
    }
    this.prev = cpus;
  }
}

/* ------------------------------------ sampler ----------------------------------------- */
const OPS = { ">": (a, b) => a > b, "<": (a, b) => a < b, ">=": (a, b) => a >= b, "<=": (a, b) => a <= b };

function evalAlerts(gpus) {
  const rules = Array.isArray(config.alerts) ? config.alerts : [];
  const out = [];
  for (const r of rules) {
    for (const g of gpus) {
      if (r.gpu !== "*" && r.gpu !== g.id) continue;
      const v = g.allMetrics[r.metric];
      if (!Number.isFinite(v)) continue;
      out.push({
        gpu: g.id, name: g.name, metric: r.metric, op: r.op,
        threshold: r.value, current: v, active: OPS[r.op](v, r.value),
      });
    }
  }
  return out;
}

function buildFrame() {
  const gpus = [...inventory.values()].map((g) => {
    const l = latest.get(g.id);
    const src = Object.fromEntries(Object.entries({ ...l?.system, ...l?.metrics }).filter(([, v]) => Number.isFinite(v)));
    const assigned = config.assignments.perGpu?.[g.id] || (g.vendor === "system" ? Object.keys(src) : config.assignments.default);
    const metrics = {};
    for (const k of assigned) {
      if (k in src && src[k] !== undefined && Number.isFinite(src[k])) metrics[k] = src[k];
    }
    return { id: g.id, vendor: g.vendor, name: (l && l.name) || g.name, state: l ? (l.state || "ok") : "offline", metrics, allMetrics: src, logicalLoads: l?.logicalLoads };
  });
  return { type: "telemetry", t: Date.now(), pollHz: config.pollHz, gpus, alerts: evalAlerts(gpus) };
}

/* ------------------------------------ server ------------------------------------------ */
const localHosts = new Set(["127.0.0.1:" + PORT, "localhost:" + PORT]);
function isLocalRequest(req) {
  const { host, origin } = req.headers;
  return localHosts.has(host) && (!origin || origin === "http://" + host);
}
const server = http.createServer((req, res) => {
  if (!isLocalRequest(req)) { res.writeHead(403); res.end(); return; }
  const pathname = req.url.split("?", 1)[0];
  if (req.method === "GET" && dashboardAssets.has(pathname)) {
    const asset = dashboardAssets.get(pathname);
    res.writeHead(200, { "Content-Type": asset.type + "; charset=utf-8" });
    res.end(asset.body);
    return;
  }
  if (req.method === "GET" && pathname === "/api/gpus") {
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(JSON.stringify([...inventory.values()]));
    return;
  }
  if (req.method === "GET" && pathname === "/api/settings") {
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(JSON.stringify({
      ...config,
      gpus: [...inventory.values()].map((g) => ({ ...g, available: [...(observed.get(g.id) || [])] })),
    }, null, 2));
    return;
  }
  if (req.method === "PUT" && pathname === "/api/settings") {
    const chunks = [];
    let bytes = 0;
    req.on("data", (chunk) => {
      bytes += chunk.length;
      if (bytes <= 65536) chunks.push(chunk);
      else if (!res.writableEnded) { res.writeHead(413); res.end("settings too large"); }
    });
    req.on("end", () => {
      if (res.writableEnded) return;
      try {
        const next = JSON.parse(Buffer.concat(chunks).toString("utf8"));
        const perGpu = next?.assignments?.perGpu;
        const metricKeys = list => Array.isArray(list) && list.every(k => typeof k === "string");
        if (!metricKeys(next?.assignments?.default) || !perGpu || Array.isArray(perGpu) ||
            typeof perGpu !== "object" || !Object.values(perGpu).every(metricKeys) || !Number.isFinite(next.pollHz)) {
          res.writeHead(400); res.end("invalid settings"); return;
        }
        const alerts = Array.isArray(next.alerts)
          ? next.alerts.slice(0, 50)
              .map((a) => ({ gpu: String(a.gpu || "*"), metric: String(a.metric || ""), op: Object.hasOwn(OPS, a.op) ? a.op : ">", value: Number(a.value) }))
              .filter((a) => a.metric && Number.isFinite(a.value))
          : [];
        const updated = { pollHz: Math.max(1, Math.min(10, next.pollHz)), assignments: next.assignments, alerts };
        const temporary = CONFIG_PATH + "." + process.pid + ".tmp";
        fs.writeFileSync(temporary, JSON.stringify(updated, null, 2));
        fs.renameSync(temporary, CONFIG_PATH);
        config = updated;
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify(config, null, 2));
      } catch (e) {
        res.writeHead(400); res.end("bad json");
      }
    });
    return;
  }
  res.writeHead(404); res.end("not found");
});

const wss = new WebSocketServer({ server, path: "/ws", verifyClient: ({ req }) => isLocalRequest(req) });
wss.on("connection", (ws) => ws.send(JSON.stringify(buildFrame())));
(function loop() {
  if (wss.clients.size > 0) {
    const f = JSON.stringify(buildFrame());
    for (const c of wss.clients) { try { c.send(f); } catch {} }
  }
  setTimeout(loop, 1000 / Math.max(1, Math.min(10, config.pollHz || 1)));
})();

/* -------------------------------------- UI --------------------------------------------- */
const dashboardAssets = new Map();
for (const [name, type] of [["index.html", "text/html"], ["dashboard.css", "text/css"],
                           ["themes.css", "text/css"], ["brand.svg", "image/svg+xml"],
                           ["dashboard.js", "text/javascript"], ["vitals.js", "text/javascript"]]) {
  const body = fs.readFileSync(path.join(ROOT, "..", "ui", name), "utf8");
  dashboardAssets.set("/" + name, { type, body });
}
dashboardAssets.set("/", dashboardAssets.get("/index.html"));

server.on("error", (e) => {
  console.error(`[System Bio Mon] Could not listen on port ${PORT}. ${e.message}`);
  process.exit(1);
});

/* -------------------------------------- boot ------------------------------------------- */
server.listen(PORT, "127.0.0.1", () => {
  console.log(`[System Bio Mon] http://127.0.0.1:${PORT} (pid ${process.pid})`);
});
const nvidia = new NvidiaAdapter();
nvidia.startPolling();
const amd = new AmdAdapter();
new SystemAdapter();
// The streaming helpers belong to this daemon, including when the user ends it with Ctrl+C.
process.on("exit", () => { nvidia.proc?.kill(); amd.proc?.kill(); });
process.once("SIGINT", () => process.exit(0));
process.once("SIGTERM", () => process.exit(0));
