"use strict";
const Vitals = (() => {
  const themes = {
    cyberpunk: "Cyberpunk", dark: "Dark", light: "Light",
    retro: "Retro – Atari 2600", noir: "Noir", amoled: "AMOLED",
  };
  const themeKey = "sbm-theme";
  function applyTheme(choose) {
    let theme = choose;
    try {
      if (Object.hasOwn(themes, choose)) localStorage.setItem(themeKey, choose);
      else theme = localStorage.getItem(themeKey);
    } catch {}
    document.documentElement.dataset.theme = Object.hasOwn(themes, theme)
      ? theme
      : "cyberpunk";
  }
  applyTheme();
  window.addEventListener("storage", (event) => {
    if (event.key === themeKey || event.key === null) {
      applyTheme();
      window.dispatchEvent(new Event("themechange"));
    }
  });
  // Established desktop metric names. Unknown sensors stay visible with readable labels.
  const metrics = {
    usage: ["GPU load", "%"],
    cpu: ["CPU load", "%"],
    cpuReportedClock: ["OS-reported clock", "MHz"],
    busiestThread: ["Busiest logical processor", "%"],
    temperature: ["Temperature", "°C"],
    cpuTemp: ["Package temperature", "°C"],
    power: ["Power", "W"],
    cpuPower: ["Package power", "W"],
    vramUsed: ["GPU memory", "MiB"],
    ram: ["Memory used", "MiB"],
    vramTotal: ["GPU memory total", "MiB"],
    ramTotal: ["Memory total", "MiB"],
    hotspot: ["Hotspot", "°C"],
    clock: ["Core clock", "MHz"],
    memClock: ["Memory clock", "MHz"],
    boardPower: ["Board power", "W"],
    voltage: ["Voltage", "mV"],
    fanPercent: ["Fan", "%"],
    fan: ["Fan speed", "RPM"],
    cores: ["Logical processors", ""],
    physicalCores: ["Physical cores", ""],
    cpuClock: ["Sensor clock", "MHz"],
    sharedUsed: ["Shared GPU memory", "MiB"],
    sharedTotal: ["Shared memory total", "MiB"],
    batteryCharge: ["Battery charge", "%"],
    chargePower: ["Charge power", "W"],
    dischargePower: ["Discharge power", "W"],
    boardTemp: ["Board temperature", "°C"],
  };
  const priority = Object.keys(metrics);
  const numbers = new Intl.NumberFormat(undefined, {
    maximumFractionDigits: 1,
  });
  const shortNames = {
    usage: "Load",
    cpu: "Load",
    cpuReportedClock: "Clock",
    busiestThread: "Peak thread",
    temperature: "Temp",
    cpuTemp: "Temp",
    cpuPower: "Power",
    vramUsed: "VRAM",
    ram: "RAM",
    vramTotal: "VRAM total",
    ramTotal: "RAM total",
  };
  function info(key) {
    return (
      metrics[key] || [
        key
          .replace(/([a-z])([A-Z])/g, "$1 $2")
          .replaceAll("_", " ")
          .replace(/^./, (c) => c.toUpperCase()),
        "",
      ]
    );
  }
  function keys(data) {
    const rank = (k) =>
      priority.includes(k) ? priority.indexOf(k) : priority.length;
    return Object.keys(data || {})
      .filter((k) => Number.isFinite(data[k]))
      .sort((a, b) => rank(a) - rank(b) || a.localeCompare(b));
  }
  function display(key, value) {
    let unit = info(key)[1];
    if (!Number.isFinite(value)) return ["—", ""];
    if (unit === "MiB" && Math.abs(value) >= 1024) {
      value /= 1024;
      unit = "GiB";
    }
    return [numbers.format(value), unit];
  }
  function deviceName(g) {
    const group = deviceKind(g);
    if (
      group === "System" ||
      (group === "Battery" && /^primary$/i.test(g.name || ""))
    )
      return group;
    return (g.name || g.id)
      .replace(/^NVIDIA (GeForce )?/i, "")
      .replace(/^AMD /i, "")
      .replace(/\(TM\)|\(R\)/gi, "")
      .trim();
  }
  function deviceKind(g) {
    if (g.vendor !== "system") return g.vendor;
    if (g.id.startsWith("battery-")) return "Battery";
    if (g.id.startsWith("cpu-")) return "CPU";
    if (g.id.startsWith("board-")) return "Motherboard";
    return "System";
  }
  function stateText(card, now) {
    if (card.state === "ok") return "Live";
    const age = card.sampledAt
      ? " · " + Math.max(0, Math.floor((now - card.sampledAt) / 1000)) + "s ago"
      : "";
    return (card.state === "stale" ? "Stale" : "Offline") + age;
  }
  const all = (g) => g.allMetrics || g.metrics || {};
  const frameMetrics = [
    ["fps", "FPS", "Average frames presented per second over the last second."],
    [
      "low1",
      "1% low",
      "FPS calculated from the slowest 1% of frames in the last 10 seconds. Needs at least 100 frame intervals.",
    ],
    [
      "frameMs",
      "Frame time",
      "Mean time between frame presentations over the last second, in milliseconds.",
    ],
  ];
  function gameReadout(root, compact = false) {
    const title = make("strong", "game-title");
    const status = make("span", "game-status");
    const values = make("div", "game-values");
    const rows = frameMetrics.map(([key, label, description]) => {
      const row = make("div", "game-metric");
      row.title = description;
      const caption = make("span", "game-label", label);
      const number = make("b", "game-number");
      row.append(caption, number);
      if (key === "frameMs") row.append(make("small", "game-unit", "ms"));
      values.append(row);
      return number;
    });
    const plot = make("canvas");
    plot.setAttribute("aria-hidden", "true");
    root.append(title, values, status, plot);
    return (game, history, now, seconds, showPlot) => {
      root.hidden = !game;
      if (!game) return;
      const name = game.name || "Game";
      if (title.textContent !== name) title.textContent = name;
      root.setAttribute("aria-label", name + " frame performance");
      const live = game.status === "live";
      values.hidden = !live;
      status.hidden = live;
      plot.hidden = !live || !showPlot || !Number.isFinite(game.frameMs);
      if (!live) {
        const message =
          game.status === "permission"
            ? compact
              ? "FPS needs permission"
              : "Restart the monitor as administrator to read FPS."
            : game.status === "unavailable"
              ? "FPS capture unavailable"
              : "Measuring FPS…";
        if (status.textContent !== message) status.textContent = message;
        return;
      }
      frameMetrics.forEach(([key, label], index) => {
        const value = game[key];
        const text = Number.isFinite(value)
          ? numbers.format(key === "frameMs" ? value : Math.round(value))
          : "—";
        const number = rows[index];
        if (number.textContent === text) return;
        number.textContent = text;
        const unit = key === "frameMs" ? " milliseconds" : " frames per second";
        const reading = text === "—" ? "collecting samples" : text + unit;
        number.parentElement.setAttribute("aria-label", label + " " + reading);
      });
      if (!plot.hidden) {
        plot.title = "Frame time · last " + seconds + " seconds";
        history.draw(plot, "game-frames", "frameMs", seconds, now, true);
      }
    };
  }
  class History {
    constructor() {
      this.devices = new Map();
    }
    add(frame) {
      for (const card of frame.gpus) {
        const data = card.state === "ok" ? all(card) : null;
        const t = data ? card.sampledAt || frame.t : frame.t;
        this.sample(card.id, t, data, frame.t);
      }
      if (frame.game?.pid !== this.gamePid) {
        this.devices.delete("game-frames");
        this.gamePid = frame.game?.pid;
      }
      if (frame.game) {
        const frameMs = frame.game.frameMs;
        const live = frame.game.status === "live" && Number.isFinite(frameMs);
        this.sample("game-frames", frame.t, live ? { frameMs } : null, frame.t);
      }
      for (const [id, points] of this.devices)
        if (!points.length || points.at(-1).t < frame.t - 302000)
          this.devices.delete(id);
    }
    sample(id, t, data, now) {
      let points = this.devices.get(id);
      if (!points) {
        points = [];
        this.devices.set(id, points);
      }
      const last = points.at(-1);
      if (!last || (t > last.t && (data || last.data))) points.push({ t, data });
      let cut = 0;
      while (
        cut < points.length &&
        (points[cut].t < now - 302000 || points.length - cut > 3200)
      )
        cut++;
      if (cut) points.splice(0, cut);
    }
    draw(canvas, id, key, seconds, now, mini = false) {
      const cssWidth = canvas.clientWidth,
        cssHeight = canvas.clientHeight;
      if (!cssWidth || !cssHeight) return;
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const w = Math.round(cssWidth * dpr),
        h = Math.round(cssHeight * dpr);
      if (canvas.width !== w || canvas.height !== h) {
        canvas.width = w;
        canvas.height = h;
      }
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      const style = getComputedStyle(canvas);
      ctx.clearRect(0, 0, w, h);
      const start = now - seconds * 1000,
        points = this.devices.get(id) || [];
      let max = 0,
        min = Infinity,
        count = 0;
      for (const p of points)
        if (p.t >= start && Number.isFinite(p.data?.[key])) {
          max = Math.max(max, p.data[key]);
          min = Math.min(min, p.data[key]);
          count++;
        }
      const top = info(key)[1] === "%" ? 100 : Math.max(1, max) * 1.05;
      const bottom = Math.min(0, min);
      if (!mini) {
        ctx.strokeStyle = style.getPropertyValue("--gridline").trim();
        ctx.lineWidth = dpr;
        ctx.beginPath();
        for (const at of [0.15, 0.5, 0.85]) {
          ctx.moveTo(0, h * at);
          ctx.lineTo(w, h * at);
        }
        ctx.stroke();
      }
      ctx.strokeStyle = style.color;
      ctx.fillStyle = style.color;
      ctx.lineWidth = (mini ? 1.35 : 1.75) * dpr;
      ctx.lineJoin = "round";
      ctx.beginPath();
      let prev = 0,
        x = 0,
        y = 0;
      for (const p of points) {
        if (p.t < start) continue;
        const value = p.data?.[key];
        if (!Number.isFinite(value)) {
          prev = 0;
          continue;
        }
        x = ((p.t - start) / (seconds * 1000)) * w;
        y = h - (0.08 + (0.84 * (value - bottom)) / (top - bottom)) * h;
        if (!prev || p.t - prev > 3000) {
          ctx.moveTo(x, y);
          ctx.fillRect(x - dpr, y - dpr, 2 * dpr, 2 * dpr);
        } else ctx.lineTo(x, y);
        prev = p.t;
      }
      ctx.stroke();
      return count ? [min, max] : null;
    }
  }
  function prefs() {
    let value = {};
    try {
      value =
        JSON.parse(localStorage.getItem("gdm-overlay-prefs") || "{}") || {};
    } catch {}
    return {
      density: ["selected", "full"].includes(value.density)
        ? value.density
        : "essential",
      size: [12, 14, 17].includes(value.size) ? value.size : 12,
      graphs: value.graphs !== false,
    };
  }
  function make(tag, cls, text) {
    const node = document.createElement(tag);
    if (cls) node.className = cls;
    if (text !== undefined) node.textContent = text;
    return node;
  }
  return {
    themes,
    applyTheme,
    metrics,
    info,
    keys,
    display,
    deviceName,
    deviceKind,
    stateText,
    all,
    shortNames,
    gameReadout,
    History,
    prefs,
    make,
  };
})();
