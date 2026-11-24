"use strict";
const hud = document.getElementById("hud");
const graphics = document.getElementById("graphics");
const host = document.getElementById("host");
const columns = document.getElementById("columns");
const drawGame = Vitals.gameReadout(document.getElementById("game"), true);
const api = window.__TAURI__;
const devices = new Map();
const trends = new Vitals.History();
const captions = {
  ...Vitals.shortNames,
  usage: "Load",
  cpu: "CPU",
  batteryCharge: "Charge",
  chargePower: "In",
  dischargePower: "Draw",
  cpuReportedClock: "Clock",
  memClock: "Mem clk",
  clock: "Clock",
  sharedUsed: "Shared",
  cpuPower: "Power",
};
let preferences = Vitals.prefs();
let seconds = 60,
  current,
  lastReceipt = 0,
  lastDraw = 0;
let locked = true,
  dragging = false,
  lastWidth = 0,
  lastHeight = 0,
  resizeScheduled = false;
let gpuKeys = ["usage", "temperature", "vramUsed"];

function fit() {
  if (!api || resizeScheduled) return;
  resizeScheduled = true;
  requestAnimationFrame(() => {
    resizeScheduled = false;
    const bounds = hud.getBoundingClientRect();
    const w = Math.ceil(bounds.width),
      h = Math.ceil(bounds.height);
    if (w === lastWidth && h === lastHeight) return;
    lastWidth = w;
    lastHeight = h;
    api.core.invoke("resize_overlay", { w, h }).catch(() => {
      lastWidth = lastHeight = 0;
    });
  });
}

function setPreferences() {
  preferences = Vitals.prefs();
  document.documentElement.style.fontSize = preferences.size + "px";
  document.documentElement.style.setProperty(
    "--limit",
    Math.max(220, screen.availWidth - 16) + "px",
  );
  document.body.classList.toggle(
    "essential",
    preferences.density === "essential",
  );
  document.body.classList.toggle("graphs", preferences.graphs);
  seconds = localStorage.getItem("gdm-range") === "300" ? 300 : 60;
  draw();
  fit();
}

function kind(g) {
  if (g.vendor !== "system" && !g.id.startsWith("cpu-")) return "gpu";
  return g.id.split("-")[0];
}

function nameFor(g) {
  const short = Vitals.deviceName(g);
  switch (kind(g)) {
    case "cpu":
      return "CPU";
    case "battery":
      return "Battery";
    case "sys":
      return Number.isFinite(Vitals.all(g).cpu) ? "System" : "RAM";
    case "gpu":
      return (
        short
          .replace(/^GeForce\s+/i, "")
          .replace(/^Radeon\s+(?=(?:RX\s+)?\d)/i, "")
          .replace(/\s+Laptop GPU\b/i, "")
          .replace(/\s+Graphics$/i, "")
          .trim() || short
      );
    default:
      return short;
  }
}

function addDevice(g) {
  const section = Vitals.make("section", "device");
  const name = Vitals.make("strong", "name");
  const values = Vitals.make("div", "values");
  const status = Vitals.make("span", "status");
  const plot = Vitals.make("canvas");
  section.dataset.vendor = g.vendor;
  section.dataset.kind = kind(g);
  plot.setAttribute("aria-hidden", "true");
  section.append(name, values, status, plot);
  (kind(g) === "gpu" ? graphics : host).append(section);
  const card = { section, name, values, status, plot, rows: new Map() };
  devices.set(g.id, card);
  return card;
}

function sharedColumns(gpus) {
  const live = gpus.filter((g) => kind(g) === "gpu" && g.state === "ok");
  if (live.length) {
    const keys = new Set(live.flatMap((g) => Vitals.keys(Vitals.all(g))));
    // The first three columns have the same meaning across every GPU.
    gpuKeys = [
      ...new Set([
        "usage",
        "temperature",
        "vramUsed",
        "power",
        "clock",
        ...keys,
      ]),
    ]
      .filter((k) => keys.has(k))
      .slice(0, 3);
  }
  const labels = [
    "GPU",
    ...gpuKeys.map((k) => captions[k] || Vitals.info(k)[0]),
  ];
  if (preferences.graphs) labels.push("Trend");
  if (columns.dataset.labels !== labels.join("|")) {
    columns.dataset.labels = labels.join("|");
    columns.replaceChildren(
      ...labels.map((label) => Vitals.make("span", null, label)),
    );
    const widths = gpuKeys.map(
      (k) =>
        ({ usage: "3.1em", temperature: "3.4em", vramUsed: "5.3em" })[k] ||
        "5em",
    );
    graphics.style.setProperty("--readout-cols", widths.join(" "));
  }
}

function essentialKeys(g, data) {
  if (kind(g) === "gpu") return gpuKeys;
  const available = Vitals.keys(data);
  const important = [
    "cpu",
    "cpuTemp",
    "cpuPower",
    "ram",
    "batteryCharge",
    "chargePower",
    "dischargePower",
    "boardTemp",
  ];
  const keys = important.filter((k) => available.includes(k));
  return (keys.length ? keys : available).slice(0, 3);
}

function metricLabel(g, key) {
  const essential = preferences.density === "essential";
  const device = kind(g);
  if (
    essential &&
    ((device === "gpu" && ["usage", "temperature"].includes(key)) ||
      (device === "battery" && key === "batteryCharge") ||
      (device === "cpu" && key === "cpu") ||
      (device === "sys" && key === "ram" && nameFor(g) === "RAM"))
  )
    return "";
  return captions[key] || Vitals.info(key)[0];
}

function readings(card, g, live) {
  const essential = preferences.density === "essential";
  const data = preferences.density === "selected" ? g.metrics : Vitals.all(g);
  if (live) {
    const keys = essential ? essentialKeys(g, data) : Vitals.keys(data);
    const signature = [preferences.density, ...keys].join("|");
    if (card.signature !== signature) {
      card.signature = signature;
      card.values.replaceChildren();
      card.rows.clear();
      for (const key of keys) {
        const group = Vitals.make("span", "reading");
        group.dataset.metric = key;
        const number = Vitals.make("b", "number"),
          unit = Vitals.make("small", "unit");
        const label = metricLabel(g, key);
        group.title = Vitals.info(key)[0];
        if (label) group.append(Vitals.make("span", "caption", label));
        group.append(number, unit);
        card.values.append(group);
        card.rows.set(key, { number, unit, group });
      }
    }
  }
  for (const [key, row] of card.rows) {
    let value = live ? data[key] : null;
    if (
      essential &&
      Number.isFinite(value) &&
      (["%", "°C"].includes(Vitals.info(key)[1]) ||
        (Vitals.info(key)[1] === "MiB" && value < 1024))
    )
      value = Math.round(value);
    const [n, u] = Vitals.display(key, value);
    if (row.number.textContent !== n || row.unit.textContent !== u) {
      row.number.textContent = n;
      row.unit.textContent = u;
      row.group.setAttribute(
        "aria-label",
        Vitals.info(key)[0] + " " + n + " " + u,
      );
    }
  }
}

function draw() {
  if (!current || document.hidden) return;
  const lag = performance.now() - lastReceipt,
    disconnected = lag > 4000,
    now = current.t + lag;
  const ids = new Set(),
    duplicates = new Set(),
    names = new Set();
  const hot = new Set(
    (current.alerts || []).filter((a) => a.active).map((a) => a.gpu),
  );
  sharedColumns(current.gpus);
  for (const g of current.gpus) {
    const label = nameFor(g);
    if (names.has(label)) duplicates.add(label);
    names.add(label);
  }
  for (const g of current.gpus) {
    ids.add(g.id);
    const card = devices.get(g.id) || addDevice(g);
    const live = g.state === "ok" && !disconnected,
      alert = live && hot.has(g.id);
    const label = nameFor(g);
    card.name.textContent = duplicates.has(label)
      ? label + " " + g.id.split("-").pop()
      : label;
    card.name.title = g.name || g.id;
    card.section.classList.toggle("off", !live);
    card.section.classList.toggle("alert", alert);
    card.section.setAttribute(
      "aria-label",
      (g.name || g.id) + (alert ? " · Alert" : ""),
    );
    card.status.textContent = !live
      ? disconnected
        ? "Disconnected"
        : Vitals.stateText(g, now)
      : "";
    card.status.hidden = live;
    readings(card, g, live);
    const data = Vitals.all(g);
    const selected = localStorage.getItem("gdm-trend-" + g.id);
    if (live)
      card.plotKey = Number.isFinite(data[selected])
        ? selected
        : essentialKeys(g, data).find((k) => Number.isFinite(data[k]));
    card.plot.hidden = !preferences.graphs || !card.plotKey || !live;
    if (!card.plot.hidden) {
      card.plot.title =
        Vitals.info(card.plotKey)[0] + " · " + seconds + " seconds";
      trends.draw(
        card.plot,
        g.id,
        card.plotKey,
        seconds,
        now,
        true,
      );
    }
  }
  for (const [id, card] of devices) {
    if (!ids.has(id)) {
      card.section.remove();
      devices.delete(id);
    }
  }
  graphics.hidden = !graphics.querySelector(".device");
  host.hidden = !host.childElementCount;
  const game = disconnected ? null : current.game;
  drawGame(game, trends, now, seconds, preferences.graphs);
  document.getElementById("waiting").hidden = ids.size > 0 || !!game;
  fit();
}
function layout(value) {
  document.body.classList.toggle("stacked", value === "v");
  draw();
  fit();
}
function opacity(value) {
  document.documentElement.style.setProperty(
    "--alpha",
    Math.max(0.15, Math.min(1, value)),
  );
}
if (api) {
  api.event.listen("telemetry", ({ payload: frame }) => {
    if (!Array.isArray(frame?.gpus)) return;
    current = frame;
    lastReceipt = performance.now();
    trends.add(frame);
    if (!lastDraw || lastReceipt - lastDraw >= 250) {
      lastDraw = lastReceipt;
      draw();
    }
  });
  api.event.listen("overlay-layout", (e) => layout(e.payload));
  api.event.listen("overlay-opacity", (e) => opacity(e.payload / 100));
  api.event.listen("overlay-mode", (e) => {
    locked = e.payload.locked;
    document.body.classList.toggle("moving", !locked);
    if (locked) endDrag();
  });
  api.core.invoke("get_overlay_layout").then(layout);
  api.core.invoke("get_overlay_opacity").then(opacity);
}
function endDrag() {
  if (!dragging) return;
  dragging = false;
  api.core.invoke("overlay_drag_end").catch(() => {});
}
hud.addEventListener("pointerdown", (e) => {
  if (locked || e.button !== 0 || !api) return;
  hud.setPointerCapture(e.pointerId);
  dragging = true;
  api.core.invoke("overlay_drag_start").catch(endDrag);
});
hud.addEventListener("pointerup", endDrag);
hud.addEventListener("lostpointercapture", endDrag);
window.addEventListener("storage", setPreferences);
window.addEventListener("focus", setPreferences);
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) setPreferences();
});
setInterval(() => {
  if (current && performance.now() - lastReceipt > 4000) draw();
}, 1500);
setPreferences();
