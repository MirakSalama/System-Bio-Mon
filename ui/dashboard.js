"use strict";
const $ = (id) => document.getElementById(id);
const { make, info, keys, display } = Vitals;
const native = window.__TAURI__;
const phone =
  !native && new URLSearchParams(location.search).get("view") === "phone";
const invoke = (command, args) => native.core.invoke(command, args);
const historyData = new Vitals.History();
const drawGame = Vitals.gameReadout($("game"));
const cards = new Map();
let frame,
  receivedAt = 0,
  timer,
  lastPaint = 0,
  activeAlerts = new Set(),
  sound,
  settings,
  selected,
  shareState;
let overlayVisible = false;
const themeControl = $("theme");
for (const [id, name] of Object.entries(Vitals.themes))
  themeControl.add(new Option(name, id));
themeControl.value = document.documentElement.dataset.theme;
themeControl.onchange = () => {
  Vitals.applyTheme(themeControl.value);
  paint();
};
window.addEventListener("themechange", () => {
  themeControl.value = document.documentElement.dataset.theme;
  paint();
});
const range = $("range"),
  view = $("view");
view.value = localStorage.getItem("gdm-mode") === "v" ? "v" : "full";
range.value = localStorage.getItem("gdm-range") === "300" ? "300" : "60";
view.onchange = () => {
  localStorage.setItem("gdm-mode", view.value);
  paint();
};
range.onchange = () => {
  localStorage.setItem("gdm-range", range.value);
  paint();
};
function notify(message) {
  $("notice").textContent = String(message);
  $("notice").hidden = false;
  clearTimeout(timer);
  timer = setTimeout(() => {
    $("notice").hidden = true;
  }, 3800);
}
function setStatus(message, live) {
  const status = $("status");
  if (status.textContent !== message) status.textContent = message;
  status.classList.toggle("live", live);
}
function createCard(id) {
  const root = make("article", "device"),
    head = make("div", "card-head"),
    title = make("div");
  const kind = make("div", "device-kind"),
    name = make("h3"),
    state = make("span", "state");
  title.append(kind, name);
  head.append(title, state);
  const grid = make("div", "metrics"),
    empty = make("p", "empty");
  const graph = make("div", "graph"),
    controls = make("div", "plot-head");
  const select = make("select"),
    label = make("label", null, "Trend");
  label.htmlFor = select.id = "plot-" + id;
  controls.append(label, select);
  const canvas = make("canvas");
  canvas.setAttribute("aria-hidden", "true");
  const caption = make("div", "plot-caption"),
    span = make("span"),
    extent = make("span");
  caption.append(span, extent);
  graph.append(controls, canvas, caption);
  root.append(head, grid, empty, graph);
  $("cards").append(root);
  select.onchange = () => {
    localStorage.setItem("gdm-trend-" + id, select.value);
    paint();
  };
  return {
    root,
    kind,
    name,
    state,
    grid,
    empty,
    graph,
    select,
    canvas,
    span,
    extent,
    rows: new Map(),
    pointKeys: [],
  };
}
function metricRows(card, g, live) {
  if (live) {
    const order = keys(g.metrics);
    if (order.join("|") !== Array.from(card.rows.keys()).join("|")) {
      card.grid.replaceChildren();
      card.rows.clear();
      for (const key of order) {
        const block = make("div", "metric"),
          name = make("div", "metric-label", info(key)[0]),
          value = make("div", "metric-value");
        const number = document.createTextNode(""),
          unit = make("small");
        value.append(number, unit);
        block.append(name, value);
        card.grid.append(block);
        card.rows.set(key, { number, unit });
      }
    }
  }
  for (const [key, els] of card.rows) {
    const [n, u] = display(key, live ? g.metrics[key] : null, g.vendor);
    if (els.number.data !== n) els.number.data = n;
    if (els.unit.textContent !== u) els.unit.textContent = u;
  }
  card.empty.hidden = card.rows.size > 0;
  card.empty.textContent = live
    ? "No display metrics selected."
    : "Waiting for a fresh reading.";
}
function plot(card, g, now) {
  if (g.state === "ok") {
    const candidates = keys(Vitals.all(g)).filter(
      (k) => !["ramTotal", "vramTotal", "cores", "physicalCores"].includes(k),
    );
    if (candidates.join("|") !== card.pointKeys.join("|")) {
      card.pointKeys = candidates;
      const previous =
        card.select.value || localStorage.getItem("gdm-trend-" + g.id);
      card.select.replaceChildren(
        ...candidates.map((k) => new Option(info(k)[0], k)),
      );
      if (candidates.includes(previous)) card.select.value = previous;
    }
  }
  card.graph.hidden = !card.select.value;
  if (card.graph.hidden || view.value === "v" || document.hidden) return;
  const seconds = Number(range.value);
  const extent = historyData.draw(
    card.canvas,
    g.id,
    card.select.value,
    seconds,
    now,
  );
  card.span.textContent = seconds === 60 ? "Last 60 seconds" : "Last 5 minutes";
  const measure = (v) =>
    display(card.select.value, v, g.vendor).filter(Boolean).join(" ");
  card.extent.textContent = extent
    ? "Min " + measure(extent[0]) + " · Max " + measure(extent[1])
    : "Waiting for samples";
}
function updateProcessorDetails(card, loads, live) {
  if (!card.threadDetails && !loads?.length) return;
  if (!card.threadDetails) {
    card.threadDetails = make("details", "processor-details");
    card.threadGrid = make("ol", "thread-grid");
    card.threadDetails.append(
      make("summary", null, "Logical processor load"),
      card.threadGrid,
    );
    card.root.append(card.threadDetails);
    card.threadDetails.ontoggle = () => {
      if (card.threadDetails.open) paint();
    };
  }
  if (!card.threadDetails.open) return;
  if (loads?.length && loads.length !== card.threadGrid.children.length) {
    const cells = loads.map((_, index) => {
      const cell = make("li");
      const meter = make("meter");
      meter.max = 100;
      meter.setAttribute("aria-hidden", "true");
      cell.append(make("span", null, "LP " + index), make("b"), meter);
      return cell;
    });
    card.threadGrid.replaceChildren(...cells);
  }
  Array.from(card.threadGrid.children).forEach((cell, index) => {
    const value = live ? loads?.[index] : undefined;
    const meter = cell.lastElementChild;
    const number = cell.children[1];
    const text = Number.isFinite(value) ? Math.round(value) + "%" : "—";
    if (number.textContent !== text) number.textContent = text;
    meter.hidden = !Number.isFinite(value);
    if (!meter.hidden && meter.value !== value) meter.value = value;
  });
}
function paint() {
  if (!frame) return;
  const now = frame.t + Math.max(0, performance.now() - receivedAt);
  const disconnected = performance.now() - receivedAt > 4000;
  drawGame(
    disconnected ? null : frame.game,
    historyData,
    now,
    Number(range.value),
    !document.hidden,
  );
  $("cards").classList.toggle("compact", view.value === "v");
  const hot = new Set(
    (disconnected ? [] : frame.alerts || [])
      .filter((a) => a.active)
      .map((a) => a.gpu),
  );
  const existing = new Set();
  for (const g of frame.gpus) {
    existing.add(g.id);
    const card = cards.get(g.id) || createCard(g.id);
    cards.set(g.id, card);
    const live = !disconnected && g.state === "ok";
    card.root.dataset.vendor = g.vendor;
    card.root.dataset.kind = Vitals.deviceKind(g).toLowerCase();
    card.root.dataset.alert = String(hot.has(g.id));
    card.kind.textContent = Vitals.deviceKind(g);
    card.name.textContent = g.name || g.id;
    card.state.className = "state " + (live ? "live" : "warn");
    card.state.textContent = disconnected
      ? "Disconnected"
      : Vitals.stateText(g, now);
    metricRows(card, g, live);
    plot(card, g, now);
    if (g.id === "cpu-0") updateProcessorDetails(card, g.logicalLoads, live);
  }
  for (const [id, c] of cards)
    if (!existing.has(id)) {
      c.root.remove();
      cards.delete(id);
    }
  alerts(disconnected ? [] : frame.alerts || []);
  if (disconnected)
    setStatus("Disconnected · waiting for fresh readings", false);
}
function alerts(rules) {
  const active = rules.filter((a) => a.active),
    signatures = active.map((a) =>
      JSON.stringify([a.gpu, a.metric, a.op, a.threshold]),
    );
  const box = $("alerts");
  box.hidden = !active.length;
  if (active.length) {
    box.replaceChildren(
      make("strong", null, "Threshold alerts"),
      ...active.map((a) => {
        const measure = (v) => display(a.metric, v).filter(Boolean).join(" ");
        return make(
          "div",
          null,
          (a.name || a.gpu) +
            " · " +
            info(a.metric)[0] +
            " " +
            measure(a.current) +
            " " +
            a.op +
            " " +
            measure(a.threshold),
        );
      }),
    );
  }
  if (
    sound?.state === "running" &&
    !phone &&
    signatures.some((s) => !activeAlerts.has(s))
  ) {
    const wave = sound.createOscillator(),
      gain = sound.createGain();
    wave.frequency.value = 800;
    gain.gain.setValueAtTime(0.04, sound.currentTime);
    gain.gain.exponentialRampToValueAtTime(0.001, sound.currentTime + 0.2);
    wave.connect(gain);
    gain.connect(sound.destination);
    wave.start();
    wave.stop(sound.currentTime + 0.2);
  }
  activeAlerts = new Set(signatures);
}
document.addEventListener(
  "pointerdown",
  () => {
    if (!phone) {
      try {
        sound = new AudioContext();
        sound.resume().catch(() => {});
      } catch {}
    }
  },
  { once: true },
);
function receive(f) {
  if (
    f?.type !== "telemetry" ||
    !Number.isFinite(f.t) ||
    !Array.isArray(f.gpus)
  )
    return;
  if (frame?.t === f.t) return;
  receivedAt = performance.now();
  frame = f;
  historyData.add(f);
  if (document.hidden || (receivedAt - lastPaint < 240 && lastPaint)) return;
  lastPaint = receivedAt;
  const count = f.gpus.filter((g) => g.state === "ok").length;
  setStatus(
    count +
      " of " +
      f.gpus.length +
      " sources live · " +
      new Date(f.t).toLocaleTimeString(),
    count > 0,
  );
  paint();
}
for (const dialog of document.querySelectorAll("dialog")) {
  for (const btn of dialog.querySelectorAll(".close"))
    btn.onclick = () => dialog.close();
  dialog.addEventListener("click", (e) => {
    if (e.target === dialog) {
      const b = dialog.getBoundingClientRect();
      if (
        e.clientX < b.left ||
        e.clientX > b.right ||
        e.clientY < b.top ||
        e.clientY > b.bottom
      )
        dialog.close();
    }
  });
}
function wsConnect() {
  const ws = new WebSocket(
    (location.protocol === "https:" ? "wss://" : "ws://") +
      location.host +
      "/ws",
  );
  ws.onmessage = (e) => {
    try {
      receive(JSON.parse(e.data));
    } catch {}
  };
  ws.onclose = () => {
    setStatus("Reconnecting to the monitor…", false);
    setTimeout(wsConnect, 1500);
  };
}
async function phoneConnect() {
  // URL fragments stay on the device. Store the key for refreshes in this tab only.
  let token =
    location.hash.slice(1) || sessionStorage.getItem("gdm-pair") || "";
  if (location.hash) {
    if (/^[a-f0-9]{32}$/i.test(token))
      sessionStorage.setItem("gdm-pair", token);
    history.replaceState(null, "", location.pathname + location.search);
  }
  if (!/^[a-f0-9]{32}$/i.test(token)) {
    setStatus("Scan the pairing code on your PC to connect.", false);
    return;
  }
  async function poll() {
    let again = true;
    try {
      const response = await fetch("/api/telemetry", {
        headers: { Authorization: "Bearer " + token },
        cache: "no-store",
        signal: AbortSignal.timeout(4000),
      });
      if (response.status === 401 || response.status === 403) {
        sessionStorage.removeItem("gdm-pair");
        again = false;
        frame = null;
        drawGame(null);
        cards.clear();
        $("cards").replaceChildren();
        alerts([]);
        setStatus("Pairing ended. Scan the new code on your PC.", false);
      } else if (response.status === 503)
        setStatus("Connected · waiting for sensors…", false);
      else if (response.ok) receive(await response.json());
      else setStatus("Monitor unavailable · reconnecting…", false);
    } catch {
      setStatus("Connection lost · reconnecting…", false);
    }
    if (again) setTimeout(poll, document.hidden ? 3000 : 1000);
  }
  await poll();
}
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) paint();
});
setInterval(() => {
  if (!document.hidden && frame && performance.now() - receivedAt > 4000)
    paint();
}, 1500);
window.addEventListener("resize", () => {
  if (frame) paint();
});

function checkLabel(text, checked, onchange) {
  const label = make("label"),
    input = make("input");
  input.type = "checkbox";
  input.checked = checked;
  input.onchange = () => onchange(input.checked);
  label.append(input, document.createTextNode(text));
  return label;
}
function renderMetrics() {
  const container = $("metrics-settings");
  container.replaceChildren();
  const available = Array.from(
    new Set([
      ...Object.keys(Vitals.metrics),
      ...settings.gpus.flatMap((g) => g.available || []),
      ...selected.assignments.default,
    ]),
  );
  function choices(section, options, chosen) {
    const box = make("div", "choices");
    for (const key of options)
      box.append(
        checkLabel(info(key)[0], chosen.includes(key), (on) => {
          if (on && !chosen.includes(key)) chosen.push(key);
          if (!on && chosen.includes(key))
            chosen.splice(chosen.indexOf(key), 1);
        }),
      );
    section.append(box);
  }
  function group(title) {
    const details = make("details");
    details.append(make("summary", null, title));
    container.append(details);
    return details;
  }
  choices(group("Default metrics"), available, selected.assignments.default);
  for (const g of settings.gpus) {
    const section = group(g.name || g.id);
    const inherited = !Object.hasOwn(selected.assignments.perGpu, g.id);
    const defaults =
      g.vendor === "system" ? g.available || [] : selected.assignments.default;
    const inherit = checkLabel(
      g.vendor === "system"
        ? "Show all available metrics"
        : "Use default metrics",
      inherited,
      (on) => {
        if (on) delete selected.assignments.perGpu[g.id];
        else selected.assignments.perGpu[g.id] = [...defaults];
        renderMetrics();
        const replacement = Array.from(container.children)[
          settings.gpus.indexOf(g) + 1
        ];
        replacement.open = true;
      },
    );
    inherit.className = "inherit";
    section.append(inherit);
    if (!inherited)
      choices(
        section,
        [
          ...new Set([
            ...(g.available || []),
            ...selected.assignments.perGpu[g.id],
          ]),
        ],
        selected.assignments.perGpu[g.id],
      );
  }
}
function selectControl(label, options, value, onchange) {
  const select = make("select");
  select.setAttribute("aria-label", label);
  for (const [text, key] of options) select.add(new Option(text, key));
  select.value = value;
  select.onchange = () => onchange(select.value);
  return select;
}
function renderRules() {
  const area = $("rules");
  area.replaceChildren();
  if (!selected.alerts.length)
    area.append(make("p", "empty", "No alerts configured."));
  for (const [i, a] of selected.alerts.entries()) {
    const line = make("div", "rule"),
      devices = [
        ["Any card", "*"],
        ...settings.gpus.map((g) => [g.name || g.id, g.id]),
      ];
    if (!devices.some(([, id]) => a.gpu === id)) devices.push([a.gpu, a.gpu]);
    const available = [
      ...new Set([
        a.metric,
        ...Object.keys(Vitals.metrics),
        ...settings.gpus.flatMap((g) => g.available || []),
      ]),
    ];
    const input = make("input");
    input.type = "number";
    input.step = "any";
    input.value = a.value;
    input.setAttribute("aria-label", "Alert threshold");
    input.required = true;
    input.oninput = () => {
      a.value = input.valueAsNumber;
    };
    const remove = make("button", null, "×");
    remove.setAttribute("aria-label", "Remove alert " + (i + 1));
    remove.onclick = () => {
      selected.alerts.splice(i, 1);
      renderRules();
    };
    line.append(
      selectControl("Alert source", devices, a.gpu, (v) => (a.gpu = v)),
      selectControl(
        "Alert metric",
        available.map((k) => [info(k)[0], k]),
        a.metric,
        (v) => (a.metric = v),
      ),
      selectControl(
        "Comparison",
        ["<", "<=", ">", ">="].map((op) => [op, op]),
        a.op,
        (v) => (a.op = v),
      ),
      input,
      remove,
    );
    area.append(line);
  }
}
async function openSettings() {
  try {
    settings = native
      ? await invoke("get_settings")
      : await (await fetch("/api/settings")).json();
    selected = structuredClone({
      assignments: settings.assignments,
      alerts: settings.alerts || [],
    });
    $("hz").replaceChildren(
      ...Array.from({ length: 10 }, (_, i) => new Option(i + 1 + " Hz", i + 1)),
    );
    $("hz").value = settings.pollHz;
    renderMetrics();
    renderRules();
    $("settings-dialog").showModal();
    if (native) {
      const installed = await invoke("sensor_driver_installed").catch(
        () => null,
      );
      const area = $("sensor-settings");
      area.replaceChildren();
      if (installed === false) {
        area.append(
          make("h3", null, "Advanced sensors"),
          make(
            "p",
            "empty",
            "The optional PawnIO driver enables supported CPU temperatures, power and motherboard fans. Windows may ask for administrator approval.",
          ),
        );
        const enable = make("button", null, "Enable advanced sensors");
        area.append(enable);
        enable.onclick = async () => {
          enable.disabled = true;
          try {
            await invoke("enable_sensor_driver");
            area.replaceChildren(
              make(
                "p",
                null,
                "Restart the monitor after driver setup finishes.",
              ),
            );
          } catch (e) {
            notify(e);
            enable.disabled = false;
          }
        };
      } else if (installed === true)
        area.append(make("p", "empty", "Advanced sensor driver installed."));
    }
  } catch (e) {
    notify("Could not open settings. " + e);
  }
}
$("settings-btn").onclick = openSettings;
$("add-rule").onclick = () => {
  if (selected.alerts.length >= 50) {
    notify("The limit is 50 alert rules.");
    return;
  }
  selected.alerts.push({ gpu: "*", metric: "temperature", op: ">", value: 85 });
  renderRules();
};
$("save").onclick = async () => {
  if (selected.alerts.some((a) => !Number.isFinite(a.value))) {
    notify("Enter a number for each alert threshold.");
    return;
  }
  const body = { ...selected, pollHz: Number($("hz").value) };
  try {
    if (native) await invoke("save_settings", { s: body });
    else {
      const r = await fetch("/api/settings", {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      if (!r.ok) throw Error("Server rejected settings.");
    }
    $("settings-dialog").close();
    notify("Settings saved");
  } catch (e) {
    notify("Could not save settings. " + e);
  }
};
function configureOverlay() {
  const prefs = Vitals.prefs();
  $("ov-density").value = prefs.density;
  $("ov-size").value = prefs.size;
  $("ov-graphs").checked = prefs.graphs;
  for (const id of ["ov-density", "ov-size", "ov-graphs"])
    $(id).onchange = () => {
      localStorage.setItem(
        "gdm-overlay-prefs",
        JSON.stringify({
          density: $("ov-density").value,
          size: Number($("ov-size").value),
          graphs: $("ov-graphs").checked,
        }),
      );
    };
  $("overlay-btn").onclick = async () => {
    try {
      const d = await invoke("get_settings");
      overlayVisible = !!d.overlayVisible;
      $("ov-show").checked = overlayVisible;
      $("ov-game-only").checked = !!d.overlayGameOnly;
      $("ov-game-only").disabled = !overlayVisible;
      $("ov-lock").disabled = !overlayVisible;
      $("ov-layout").value = d.overlayLayout || "h";
      $("ov-opacity").value = Math.round((d.overlayOpacity ?? 1) * 100);
      $("opacity-out").value = $("ov-opacity").value + "%";
      $("overlay-dialog").showModal();
    } catch (e) {
      notify(e);
    }
  };
  $("ov-game-only").onchange = async () => {
    const check = $("ov-game-only");
    const enabled = check.checked;
    check.disabled = true;
    try {
      await invoke("set_overlay_game_only", { enabled });
    } catch (e) {
      check.checked = !enabled;
      notify(e);
    } finally {
      check.disabled = !overlayVisible;
    }
  };
  $("ov-show").onchange = async () => {
    const check = $("ov-show");
    check.disabled = true;
    try {
      await invoke("show_overlay", { on: check.checked });
      overlayVisible = check.checked;
      $("ov-lock").checked = true;
      $("ov-lock").disabled = !overlayVisible;
      $("ov-game-only").disabled = !overlayVisible;
    } catch (e) {
      check.checked = overlayVisible;
      notify(e);
    } finally {
      check.disabled = false;
    }
  };
  $("ov-lock").onchange = async () => {
    try {
      await invoke("overlay_lock", { locked: $("ov-lock").checked });
    } catch (e) {
      $("ov-lock").checked = !$("ov-lock").checked;
      notify(e);
    }
  };
  $("ov-layout").onchange = () =>
    invoke("set_overlay_layout", { layout: $("ov-layout").value }).catch(
      notify,
    );
  $("ov-position").onchange = async () => {
    try {
      await invoke("move_overlay_preset", { pos: $("ov-position").value });
    } catch (e) {
      notify(e);
    } finally {
      $("ov-position").value = "";
    }
  };
  $("ov-opacity").oninput = () => {
    $("opacity-out").value = $("ov-opacity").value + "%";
  };
  $("ov-opacity").onchange = () =>
    invoke("set_overlay_opacity", { v: Number($("ov-opacity").value) }).catch(
      notify,
    );
  $("pin").onchange = () =>
    invoke("set_always_on_top", { flag: $("pin").checked }).catch(notify);
}
function renderSharing(state) {
  shareState = state;
  $("share-toggle").textContent = state.active
    ? "Stop sharing"
    : "Start sharing";
  $("share-info").textContent = state.active
    ? "Scan on a device connected to the same network. Closing the dashboard keeps sharing active in the tray. Use Quit in the tray to stop sharing and exit."
    : "Share a read-only view with a phone or tablet on the same network.";
  $("share-details").hidden = !state.active;
  $("pair-url").value = state.active ? state.url : "";
  if (state.active)
    $("qr").src =
      "data:image/svg+xml;charset=utf-8," + encodeURIComponent(state.qrSvg);
  else $("qr").removeAttribute("src");
}
$("share-btn").onclick = async () => {
  try {
    renderSharing(await invoke("get_sharing"));
    $("share-dialog").showModal();
  } catch (e) {
    notify(e);
  }
};
$("share-toggle").onclick = async () => {
  $("share-toggle").disabled = true;
  try {
    renderSharing(
      await invoke(shareState.active ? "stop_sharing" : "start_sharing"),
    );
  } catch (e) {
    $("share-info").textContent = String(e);
  } finally {
    $("share-toggle").disabled = false;
  }
};
if (native) {
  for (const id of ["overlay-btn", "share-btn", "settings-btn", "pin-field"])
    $(id).hidden = false;
  configureOverlay();
  native.event
    .listen("telemetry", (e) => receive(e.payload))
    .catch((e) => setStatus("Could not connect to sensors. " + e, false));
} else if (phone) phoneConnect();
else {
  $("settings-btn").hidden = false;
  wsConnect();
}
