#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod adlx;
mod frame_capture;
mod game_detection;
mod memory;
mod sensors;
mod sharing;
mod tray;

use std::collections::{BTreeMap, HashMap};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Number, Value};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

const DEFAULT_CONFIG: &str = r#"{
  "pollHz": 1,
  "assignments": {
    "default": ["usage", "temperature", "power", "vramUsed", "clock"],
    "perGpu": {
      "amd-0": ["usage", "temperature", "power", "vramUsed", "clock", "voltage", "memClock"],
      "nv-0": ["usage", "temperature", "power", "vramUsed", "vramTotal", "clock", "memClock", "fan", "fanPercent"],
      "sys-0": ["ram", "ramTotal"]
    }
  },
  "alerts": [{ "gpu": "*", "metric": "temperature", "op": ">", "value": 85 }]
}"#;

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
struct OverlayPos {
    x: f64,
    y: f64,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Config {
    poll_hz: u32,
    assignments: Assignments,
    #[serde(default)]
    alerts: Vec<Alert>,
    #[serde(default = "default_overlay_visible")]
    overlay_visible: bool,
    #[serde(default)]
    overlay_game_only: bool,
    #[serde(default)]
    overlay_pos: Option<OverlayPos>,
    #[serde(default)]
    overlay_preset: Option<String>,
    #[serde(default = "default_overlay_opacity")]
    overlay_opacity: f64,
    #[serde(default = "default_overlay_layout")]
    overlay_layout: String,
}

impl Default for Config {
    fn default() -> Self {
        serde_json::from_str(DEFAULT_CONFIG).expect("built-in default config")
    }
}

impl Config {
    fn should_show_overlay(&self, game_in_front: bool) -> bool {
        self.overlay_visible && (!self.overlay_game_only || game_in_front)
    }

    fn move_cpu_settings(&mut self) {
        let per_device = &mut self.assignments.per_gpu;
        if let Some(system) = per_device.get_mut("sys-0") {
            let moved: Vec<_> = system
                .iter()
                .filter(|k| matches!(k.as_str(), "cpu" | "cores"))
                .cloned()
                .collect();
            system.retain(|k| !matches!(k.as_str(), "cpu" | "cores"));
            if let Some(cpu) = per_device.get_mut("cpu-0") {
                for key in moved {
                    if !cpu.contains(&key) {
                        cpu.push(key);
                    }
                }
            } else if moved.len() == 1 {
                let mut selected = [
                    "physicalCores",
                    "cpuReportedClock",
                    "busiestThread",
                    "cpuTemp",
                    "cpuPower",
                    "cpuClock",
                ]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>();
                selected.extend(moved);
                per_device.insert("cpu-0".into(), selected);
            }
        }
        for alert in &mut self.alerts {
            if alert.gpu == "sys-0" && matches!(alert.metric.as_str(), "cpu" | "cores") {
                alert.gpu = "cpu-0".into();
            }
        }
    }
}

fn default_overlay_visible() -> bool {
    true
}

fn default_overlay_opacity() -> f64 {
    1.0
}

fn default_overlay_layout() -> String {
    "h".into()
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct Assignments {
    default: Vec<String>,
    #[serde(default)]
    per_gpu: HashMap<String, Vec<String>>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Alert {
    gpu: String,
    metric: String,
    op: String,
    value: f64,
}

// Prefer the vendor API when two readers report the same GPU.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    NvidiaSmi,
    Libre,
    Adlx,
    System,
}

#[derive(Clone)]
struct Card {
    sampled_at: u64,
    received: Instant,
    source: Source,
    supplemental: Option<(Instant, Metrics)>,
    vendor: String,
    name: String,
    metrics: BTreeMap<String, Number>,
}

struct AppState {
    config: Arc<Mutex<Config>>,
    cards: Arc<Mutex<HashMap<String, Card>>>,
    config_path: PathBuf,
    overlay_locked: AtomicBool,
    game_in_front: AtomicBool,
    capture: Mutex<Option<frame_capture::FrameCapture>>,
}

impl AppState {
    fn update_config(&self, change: impl FnOnce(&mut Config)) -> Result<(), String> {
        let mut guard = self.config.lock().unwrap();
        let mut next = guard.clone();
        change(&mut next);
        let data = serde_json::to_vec_pretty(&next).map_err(|e| e.to_string())?;
        let tmp = self
            .config_path
            .with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
        std::fs::rename(tmp, &self.config_path).map_err(|e| e.to_string())?;
        *guard = next;
        Ok(())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn num(n: f64) -> Number {
    Number::from_f64((n * 10.0).round() / 10.0).unwrap_or_else(|| Number::from(0))
}

fn apply_op(op: &str, v: f64, t: f64) -> bool {
    match op {
        ">" => v > t,
        "<" => v < t,
        ">=" => v >= t,
        "<=" => v <= t,
        _ => false,
    }
}

const NVIDIA_FIELDS: &str = "index,name,temperature.gpu,utilization.gpu,memory.used,memory.total,power.draw,clocks.current.graphics,clocks.current.memory,fan.speed";
const NVIDIA_KEYS: [(&str, usize); 8] = [
    ("temperature", 2),
    ("usage", 3),
    ("vramUsed", 4),
    ("vramTotal", 5),
    ("power", 6),
    ("clock", 7),
    ("memClock", 8),
    ("fanPercent", 9),
];

type Cards = Arc<Mutex<HashMap<String, Card>>>;
type Metrics = BTreeMap<String, Number>;
const STALE_AFTER: Duration = Duration::from_secs(3);
const OFFLINE_AFTER: Duration = Duration::from_secs(10);

fn record(
    cards: &Cards,
    id: String,
    vendor: String,
    name: String,
    metrics: Metrics,
    source: Source,
) {
    if metrics.is_empty() {
        return;
    }
    let mut cards = cards.lock().unwrap();
    let now = Instant::now();
    let mut supplemental = None;
    if let Some(current) = cards.get_mut(&id) {
        // OS and Libre can spell the same processor model differently.
        let same_device =
            current.vendor == vendor && (id == "cpu-0" || current.name.eq_ignore_ascii_case(&name));
        if current.source > source && current.received.elapsed() < STALE_AFTER {
            if same_device {
                current.supplemental = Some((now, metrics));
            }
            return;
        }
        if same_device && current.source < source && current.received.elapsed() < STALE_AFTER {
            supplemental = Some((current.received, std::mem::take(&mut current.metrics)));
        } else if same_device && current.source == source {
            supplemental = current.supplemental.take();
        }
    }
    cards.insert(
        id,
        Card {
            vendor,
            name,
            metrics,
            supplemental,
            source,
            sampled_at: now_ms(),
            received: now,
        },
    );
}

fn combined_metrics(card: &Card, now: Instant) -> Metrics {
    let mut metrics = card
        .supplemental
        .as_ref()
        .filter(|(t, _)| now.saturating_duration_since(*t) < STALE_AFTER)
        .map(|(_, m)| m.clone())
        .unwrap_or_default();
    metrics.extend(card.metrics.clone());
    metrics
}

fn poll_nvidia(cards: &Cards, executable: &std::path::Path) {
    let out = Command::new(executable)
        .creation_flags(0x08000000)
        .arg(format!("--query-gpu={NVIDIA_FIELDS}"))
        .arg("--format=csv,noheader,nounits")
        .output();
    let Ok(out) = out else { return };
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        return;
    }
    for line in text.lines() {
        let p: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if p.len() < 10 {
            continue;
        }
        let mut m = BTreeMap::new();
        for (k, i) in NVIDIA_KEYS {
            if let Ok(v) = p[i].parse::<f64>() {
                if v.is_finite() {
                    m.insert(k.to_string(), num(v));
                }
            }
        }
        if !m.keys().any(|k| k != "vramTotal") {
            continue;
        }
        record(
            cards,
            format!("nv-{}", p[0]),
            "nvidia".into(),
            p[1].to_string(),
            m,
            Source::NvidiaSmi,
        );
    }
}

fn tick_system(sys: &mut System, cards: &Cards, physical_cores: Option<usize>) -> Vec<u8> {
    sys.refresh_cpu_specifics(CpuRefreshKind::everything());
    sys.refresh_memory();
    let cpus = sys.cpus();
    let mut loads = Vec::with_capacity(cpus.len());
    if let Some(first_cpu) = cpus.first() {
        let mut metrics = Metrics::new();
        let mut frequency_sum = 0;
        let mut frequency_count = 0;
        for processor in cpus {
            loads.push(processor.cpu_usage().clamp(0.0, 100.0).round() as u8);
            let mhz = processor.frequency();
            if mhz > 0 {
                frequency_sum += mhz;
                frequency_count += 1;
            }
        }
        metrics.insert(
            "cpu".into(),
            num(sys.global_cpu_usage().clamp(0.0, 100.0) as f64),
        );
        metrics.insert("cores".into(), cpus.len().into());
        if let Some(count) = physical_cores {
            metrics.insert("physicalCores".into(), count.into());
        }
        if let Some(mean_clock) = frequency_sum.checked_div(frequency_count) {
            metrics.insert("cpuReportedClock".into(), mean_clock.into());
        }
        if let Some(peak) = loads.iter().max() {
            metrics.insert("busiestThread".into(), (*peak).into());
        }
        let model = first_cpu.brand().trim();
        record(
            cards,
            "cpu-0".into(),
            "system".into(),
            if model.is_empty() { "CPU" } else { model }.into(),
            metrics,
            Source::System,
        );
    }
    let host = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "System".into());
    let mut m = BTreeMap::new();
    m.insert("ram".into(), Number::from(sys.used_memory() / 1_048_576));
    m.insert(
        "ramTotal".into(),
        Number::from(sys.total_memory() / 1_048_576),
    );
    record(
        cards,
        "sys-0".into(),
        "system".into(),
        host,
        m,
        Source::System,
    );
    loads
}

fn nvidia_fallback(cards: Cards) {
    let Some(root) = std::env::var_os("SystemRoot") else {
        return;
    };
    let exe = PathBuf::from(root).join("System32").join("nvidia-smi.exe");
    if !exe.is_file() {
        return;
    }
    loop {
        let covered = {
            let cards = cards.lock().unwrap();
            let mut devices = cards.values().filter(|c| c.vendor == "nvidia").peekable();
            devices.peek().is_some()
                && devices
                    .all(|c| c.source > Source::NvidiaSmi && c.received.elapsed() < STALE_AFTER)
        };
        if !covered {
            poll_nvidia(&cards, &exe);
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn main_loop(
    handle: AppHandle,
    config: Arc<Mutex<Config>>,
    cards: Arc<Mutex<HashMap<String, Card>>>,
) {
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::everything())
            .with_memory(MemoryRefreshKind::everything()),
    );
    let physical_cores = sys.physical_core_count();
    let mut detector = game_detection::Detector::default();
    let state = handle.state::<AppState>();
    loop {
        let target = detector.foreground();
        let foreground = target.is_some();
        if state.game_in_front.swap(foreground, Ordering::Relaxed) != foreground {
            let app = handle.clone();
            let _ = handle.run_on_main_thread(move || {
                let _ = reconcile_overlay(&app, &app.state::<AppState>());
            });
        }
        let game = {
            let mut guard = state.capture.lock().unwrap();
            let Some(capture) = guard.as_mut() else {
                return;
            };
            let sample = capture.sample(target.as_ref().map(|game| game.pid));
            target.map(|game| {
                json!({
                    "pid": game.pid, "name": game.name,
                    "status": sample.status,
                    "fps": sample.fps, "low1": sample.low_fps, "frameMs": sample.frame_ms
                })
            })
        };
        let logical_loads = tick_system(&mut sys, &cards, physical_cores);

        let mut frame = {
            let cfg = config.lock().unwrap().clone();
            let c = cards.lock().unwrap();
            build_frame(&cfg, &c, &logical_loads)
        };
        frame["game"] = json!(game);
        handle.state::<sharing::Sharing>().publish(&frame);
        let _ = handle.emit("telemetry", frame);
        let hz = config.lock().unwrap().poll_hz.clamp(1, 10);
        thread::sleep(Duration::from_millis(1000 / hz as u64));
    }
}

fn build_frame(cfg: &Config, cards: &HashMap<String, Card>, logical_loads: &[u8]) -> Value {
    let mut entries: Vec<_> = cards.iter().collect();
    entries.sort_by_key(|(id, c)| {
        (
            match c.vendor.as_str() {
                "nvidia" => 0,
                "amd" => 1,
                "intel" => 2,
                _ if id.as_str() == "cpu-0" => 3,
                _ if id.as_str() == "sys-0" => 4,
                _ => 5,
            },
            *id,
        )
    });
    let mut gpus = Vec::new();
    let mut alerts = Vec::new();
    let now = Instant::now();
    for (id, card) in entries {
        let age = now.saturating_duration_since(card.received);
        let state = if age >= OFFLINE_AFTER {
            "offline"
        } else if age >= STALE_AFTER {
            "stale"
        } else {
            "ok"
        };
        let readings = if state == "ok" {
            combined_metrics(card, now)
        } else {
            Metrics::new()
        };
        let custom = cfg.assignments.per_gpu.get(id);
        // CPU, battery and board cards have their own metric sets, independent of GPU defaults.
        let metrics: Metrics = if card.vendor == "system" && custom.is_none() {
            readings.clone()
        } else {
            custom
                .unwrap_or(&cfg.assignments.default)
                .iter()
                .filter_map(|k| readings.get(k).map(|v| (k.clone(), v.clone())))
                .collect()
        };
        let mut device = json!({"id": id, "vendor": card.vendor, "name": card.name,
            "state": state, "sampledAt": card.sampled_at, "metrics": metrics, "allMetrics": readings});
        if id == "cpu-0" && state == "ok" && card.source == Source::System {
            device["logicalLoads"] = json!(logical_loads);
        }
        gpus.push(device);
        for rule in &cfg.alerts {
            if rule.gpu != "*" && rule.gpu != *id {
                continue;
            }
            if let Some(value) = readings.get(&rule.metric).and_then(Number::as_f64) {
                alerts.push(json!({"gpu": id, "name": card.name, "metric": rule.metric,
                    "op": rule.op, "threshold": rule.value, "current": value, "active": apply_op(&rule.op, value, rule.value)}));
            }
        }
    }
    json!({ "type": "telemetry", "t": now_ms(), "pollHz": cfg.poll_hz, "gpus": gpus, "alerts": alerts })
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> Value {
    let cfg = state.config.lock().unwrap();
    let cards = state.cards.lock().unwrap();
    let gpus: Vec<Value> = cards
        .iter()
        .map(|(id, c)| {
            json!({
                "id": id, "vendor": c.vendor, "name": c.name,
                "available": combined_metrics(c, Instant::now()).into_keys().collect::<Vec<_>>()
            })
        })
        .collect();
    json!({
        "pollHz": cfg.poll_hz,
        "assignments": cfg.assignments,
        "alerts": cfg.alerts,
        "overlayVisible": cfg.overlay_visible,
        "overlayGameOnly": cfg.overlay_game_only,
        "overlayOpacity": cfg.overlay_opacity,
        "overlayLayout": cfg.overlay_layout,
        "gpus": gpus
    })
}

#[cfg(test)]
mod telemetry_tests {
    use super::*;

    fn reading(value: u32) -> Metrics {
        BTreeMap::from([("temperature".into(), Number::from(value))])
    }

    #[test]
    fn overlay_preference_gates_foreground_game_without_overriding_master_switch() {
        let mut config: Config = serde_json::from_str(DEFAULT_CONFIG).unwrap();
        assert!(config.should_show_overlay(false));
        config.overlay_game_only = true;
        let mut reloaded: Config =
            serde_json::from_value(serde_json::to_value(config).unwrap()).unwrap();
        assert!(!reloaded.should_show_overlay(false));
        assert!(reloaded.should_show_overlay(true));
        reloaded.overlay_visible = false;
        assert!(!reloaded.should_show_overlay(true));
        assert!(!reloaded.should_show_overlay(false));
    }

    #[test]
    fn splits_host_readings_and_moves_existing_cpu_rules() {
        let mut cfg = Config::default();
        cfg.assignments
            .per_gpu
            .insert("sys-0".into(), vec!["cpu".into(), "ram".into()]);
        cfg.alerts[0].gpu = "sys-0".into();
        cfg.alerts[0].metric = "cpu".into();
        cfg.move_cpu_settings();
        assert_eq!(cfg.assignments.per_gpu["sys-0"], ["ram"]);
        assert!(cfg.assignments.per_gpu["cpu-0"].contains(&"cpu".into()));
        assert!(!cfg.assignments.per_gpu["cpu-0"].contains(&"cores".into()));
        assert_eq!(cfg.alerts[0].gpu, "cpu-0");
        let once = serde_json::to_value(&cfg).unwrap();
        cfg.move_cpu_settings();
        assert_eq!(serde_json::to_value(&cfg).unwrap(), once);

        let cards = Cards::default();
        let mut sys = System::new_with_specifics(
            RefreshKind::nothing().with_cpu(CpuRefreshKind::everything()),
        );
        let physical = sys.physical_core_count();
        let loads = tick_system(&mut sys, &cards, physical);
        let snapshot = build_frame(&cfg, &cards.lock().unwrap(), &loads);
        let cpu = &snapshot["gpus"][0];
        assert_eq!(cpu["id"], "cpu-0");
        assert_eq!(
            cpu["allMetrics"]["cores"].as_u64(),
            Some(loads.len() as u64)
        );
        assert_eq!(cpu["logicalLoads"].as_array().unwrap().len(), loads.len());
        assert!(loads.iter().all(|load| *load <= 100));
        assert_eq!(snapshot["gpus"][1]["id"], "sys-0");
        assert!(snapshot["gpus"][1]["allMetrics"].get("cpu").is_none());
        assert!(cpu["allMetrics"].get("ram").is_none());
    }

    #[test]
    fn cpu_package_sensors_follow_their_own_freshness() {
        let cards = Cards::default();
        record(
            &cards,
            "cpu-0".into(),
            "system".into(),
            "AMD Ryzen CPU".into(),
            BTreeMap::from([("cpu".into(), 12.into())]),
            Source::System,
        );
        record(
            &cards,
            "cpu-0".into(),
            "system".into(),
            "AMD Ryzen".into(),
            BTreeMap::from([("cpuTemp".into(), 47.into())]),
            Source::Libre,
        );
        let mut cards = cards.lock().unwrap();
        let cfg = Config::default();
        let live = build_frame(&cfg, &cards, &[10, 14]);
        assert_eq!(
            live["gpus"][0]["allMetrics"],
            json!({"cpu": 12, "cpuTemp": 47})
        );
        cards
            .get_mut("cpu-0")
            .unwrap()
            .supplemental
            .as_mut()
            .unwrap()
            .0 = Instant::now() - OFFLINE_AFTER;
        let expired = build_frame(&cfg, &cards, &[10, 14]);
        assert_eq!(expired["gpus"][0]["allMetrics"], json!({"cpu": 12}));
        assert_eq!(expired["gpus"][0]["logicalLoads"], json!([10, 14]));
    }

    #[test]
    fn any_card_reports_every_device_and_checks_hidden_metrics() {
        let cards = Cards::default();
        let mut cfg = Config::default();
        cfg.assignments
            .per_gpu
            .insert("amd-0".into(), vec!["clock".into()]);
        record(
            &cards,
            "nv-0".into(),
            "nvidia".into(),
            "RTX".into(),
            reading(60),
            Source::Libre,
        );
        record(
            &cards,
            "amd-0".into(),
            "amd".into(),
            "Radeon".into(),
            reading(92),
            Source::Adlx,
        );
        let cards = cards.lock().unwrap();
        let frame = build_frame(&cfg, &cards, &[]);
        assert_eq!(frame["alerts"].as_array().unwrap().len(), 2);
        assert_eq!(frame["alerts"][0]["active"], false);
        assert_eq!(frame["alerts"][1]["active"], true);
        assert_eq!(frame["gpus"][1]["metrics"], json!({}));
        cfg.alerts[0].gpu = "nv-0".into();
        assert_eq!(
            build_frame(&cfg, &cards, &[])["alerts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn stale_and_offline_samples_leave_gaps_then_recover() {
        let cards = Cards::default();
        let cfg = Config::default();
        record(
            &cards,
            "amd-0".into(),
            "amd".into(),
            "Radeon".into(),
            reading(93),
            Source::Adlx,
        );
        for (age, state) in [(4, "stale"), (11, "offline")] {
            let mut cards = cards.lock().unwrap();
            cards.get_mut("amd-0").unwrap().received = Instant::now() - Duration::from_secs(age);
            let frame = build_frame(&cfg, &cards, &[]);
            assert_eq!(frame["gpus"][0]["state"], state);
            assert_eq!(frame["gpus"][0]["allMetrics"], json!({}));
            assert_eq!(frame["gpus"][0]["metrics"], json!({}));
            assert_eq!(frame["alerts"], json!([]));
        }
        record(
            &cards,
            "amd-0".into(),
            "amd".into(),
            "Radeon".into(),
            Metrics::new(),
            Source::Adlx,
        );
        assert_eq!(
            build_frame(&cfg, &cards.lock().unwrap(), &[])["gpus"][0]["state"],
            "offline"
        );
        record(
            &cards,
            "amd-0".into(),
            "amd".into(),
            "Radeon".into(),
            reading(50),
            Source::Libre,
        );
        let frame = build_frame(&cfg, &cards.lock().unwrap(), &[]);
        assert_eq!(frame["gpus"][0]["state"], "ok");
        assert_eq!(frame["alerts"][0]["active"], false);
    }

    #[test]
    fn supplemental_readings_expire_independently_of_the_primary_gpu() {
        let cards = Cards::default();
        let cfg = Config::default();
        record(
            &cards,
            "amd-0".into(),
            "amd".into(),
            "Radeon".into(),
            reading(70),
            Source::Adlx,
        );
        let extra = BTreeMap::from([
            ("sharedUsed".into(), Number::from(128)),
            ("temperature".into(), Number::from(60)),
        ]);
        record(
            &cards,
            "amd-0".into(),
            "amd".into(),
            "Radeon".into(),
            extra,
            Source::Libre,
        );
        let mut cards = cards.lock().unwrap();
        let frame = build_frame(&cfg, &cards, &[]);
        assert_eq!(frame["gpus"][0]["allMetrics"]["sharedUsed"], 128);
        assert_eq!(frame["gpus"][0]["allMetrics"]["temperature"], 70);
        cards
            .get_mut("amd-0")
            .unwrap()
            .supplemental
            .as_mut()
            .unwrap()
            .0 = Instant::now() - OFFLINE_AFTER;
        let frame = build_frame(&cfg, &cards, &[]);
        assert!(frame["gpus"][0]["allMetrics"].get("sharedUsed").is_none());
        assert_eq!(frame["gpus"][0]["state"], "ok");
    }
}

#[tauri::command]
fn save_settings(state: State<AppState>, s: Value) -> Result<(), String> {
    let poll_hz = s["pollHz"].as_u64().unwrap_or(0);
    if !(1..=10).contains(&poll_hz) {
        return Err("invalid pollHz".into());
    }
    if !s["assignments"]["default"].is_array() || !s["assignments"]["perGpu"].is_object() {
        return Err("invalid assignments".into());
    }
    let assignments: Assignments =
        serde_json::from_value(s["assignments"].clone()).map_err(|e| e.to_string())?;
    let alerts: Vec<Alert> = s["alerts"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|a| {
            let gpu = a["gpu"].as_str()?;
            let metric = a["metric"].as_str()?;
            if metric.is_empty() {
                return None;
            }
            let op = a["op"].as_str()?;
            if !matches!(op, ">" | "<" | ">=" | "<=") {
                return None;
            }
            let value = a["value"].as_f64()?;
            if !value.is_finite() {
                return None;
            }
            Some(Alert {
                gpu: gpu.into(),
                metric: metric.into(),
                op: op.into(),
                value,
            })
        })
        .take(50)
        .collect();
    state.update_config(|cfg| {
        cfg.poll_hz = poll_hz as u32;
        cfg.assignments = assignments;
        cfg.alerts = alerts;
    })
}

#[tauri::command]
fn set_always_on_top(window: WebviewWindow, flag: bool) {
    let _ = window.set_always_on_top(flag);
}

fn overlay_window(app: &AppHandle) -> Result<tauri::WebviewWindow, String> {
    app.get_webview_window("overlay")
        .ok_or_else(|| "overlay window missing".into())
}

fn place_overlay(
    win: &WebviewWindow,
    preset: Option<&str>,
    position: Option<OverlayPos>,
    width: i32,
    height: i32,
) -> Result<(), String> {
    let monitors = win.available_monitors().map_err(|e| e.to_string())?;
    let saved_monitor = position.and_then(|p| {
        monitors.iter().find(|m| {
            let origin = m.position();
            let size = m.size();
            p.x >= origin.x as f64
                && p.y >= origin.y as f64
                && p.x < origin.x as f64 + size.width as f64
                && p.y < origin.y as f64 + size.height as f64
        })
    });
    let current = win.current_monitor().map_err(|e| e.to_string())?;
    let Some(monitor) = saved_monitor.or(current.as_ref()).or(monitors.first()) else {
        return Ok(());
    };
    let left = monitor.position().x;
    let top = monitor.position().y;
    let right = left + (monitor.size().width as i32 - width).max(0);
    let bottom = top + (monitor.size().height as i32 - height).max(0);
    let preset = preset.or(if saved_monitor.is_none() {
        Some("tc")
    } else {
        None
    });
    let (x, y) = match (preset, position) {
        (None, Some(p)) => (p.x as i32, p.y as i32),
        (key, _) => {
            let key = key.unwrap_or("tc");
            let x = if key.ends_with('l') {
                left + 16
            } else if key.ends_with('r') {
                right - 16
            } else {
                left + (right - left) / 2
            };
            (
                x,
                if key.starts_with('b') {
                    bottom - 16
                } else {
                    top + 16
                },
            )
        }
    };
    win.set_position(tauri::PhysicalPosition::new(
        x.clamp(left, right),
        y.clamp(top, bottom),
    ))
    .map_err(|e| e.to_string())
}

fn persist_overlay(app: &AppHandle, state: &AppState) -> Result<(), String> {
    let w = overlay_window(app)?;
    let pos = w.outer_position().map_err(|e| e.to_string())?;
    state.update_config(|cfg| {
        cfg.overlay_pos = Some(OverlayPos {
            x: pos.x as f64,
            y: pos.y as f64,
        })
    })?;
    Ok(())
}

#[tauri::command]
fn show_overlay(app: AppHandle, state: State<AppState>, on: bool) -> Result<(), String> {
    state.update_config(|cfg| cfg.overlay_visible = on)?;
    reconcile_overlay(&app, &state)
}

#[tauri::command]
fn set_overlay_game_only(
    app: AppHandle,
    state: State<AppState>,
    enabled: bool,
) -> Result<(), String> {
    state.update_config(|cfg| cfg.overlay_game_only = enabled)?;
    reconcile_overlay(&app, &state)
}

fn reconcile_overlay(app: &AppHandle, state: &AppState) -> Result<(), String> {
    let w = overlay_window(app)?;
    let on = state
        .config
        .lock()
        .unwrap()
        .should_show_overlay(state.game_in_front.load(Ordering::Relaxed));
    if w.is_visible().map_err(|e| e.to_string())? == on {
        return Ok(());
    }
    if on {
        let (preset, position) = {
            let cfg = state.config.lock().unwrap();
            (cfg.overlay_preset.clone(), cfg.overlay_pos)
        };
        let size = w.outer_size().map_err(|e| e.to_string())?;
        place_overlay(
            &w,
            preset.as_deref(),
            position,
            size.width as i32,
            size.height as i32,
        )?;
        w.set_always_on_top(true).map_err(|e| e.to_string())?;
        w.set_ignore_cursor_events(true)
            .map_err(|e| e.to_string())?;
        state.overlay_locked.store(true, Ordering::Relaxed);
        w.show().map_err(|e| e.to_string())?;
        let _ = app.emit("overlay-mode", json!({"locked": true}));
    } else {
        w.hide().map_err(|e| e.to_string())?;
    }
    memory::set_idle(&w, !on);
    Ok(())
}

#[tauri::command]
fn resize_overlay(app: AppHandle, state: State<AppState>, w: f64, h: f64) -> Result<(), String> {
    let win = overlay_window(&app)?;
    let width = w.max(80.0);
    let height = h.max(24.0);
    win.set_size(tauri::LogicalSize::new(width, height))
        .map_err(|e| e.to_string())?;
    if !state.overlay_locked.load(Ordering::Relaxed) {
        return Ok(());
    }
    let scale = win.scale_factor().map_err(|e| e.to_string())?;
    let (preset, position) = {
        let cfg = state.config.lock().unwrap();
        (cfg.overlay_preset.clone(), cfg.overlay_pos)
    };
    place_overlay(
        &win,
        preset.as_deref(),
        position,
        (width * scale).round() as i32,
        (height * scale).round() as i32,
    )
}

#[tauri::command]
fn overlay_lock(app: AppHandle, state: State<AppState>, locked: bool) -> Result<(), String> {
    let w = overlay_window(&app)?;
    w.set_ignore_cursor_events(locked)
        .map_err(|e| e.to_string())?;
    state.overlay_locked.store(locked, Ordering::Relaxed);
    let _ = app.emit("overlay-mode", serde_json::json!({ "locked": locked }));
    if locked {
        persist_overlay(&app, &state)?;
    }
    Ok(())
}

#[tauri::command]
fn overlay_drag_start(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    if state.overlay_locked.load(Ordering::Relaxed) {
        return Ok(());
    }
    state.update_config(|cfg| cfg.overlay_preset = None)?;
    let w = overlay_window(&app)?;
    w.start_dragging().map_err(|e| e.to_string())
}

#[tauri::command]
fn overlay_drag_end(app: AppHandle, state: State<AppState>) {
    persist_overlay(&app, &state).ok();
}

#[tauri::command]
fn set_overlay_opacity(app: AppHandle, state: State<AppState>, v: f64) -> Result<(), String> {
    let frac = v.clamp(15.0, 100.0) / 100.0;
    state.update_config(|cfg| cfg.overlay_opacity = frac)?;
    let _ = app.emit("overlay-opacity", (frac * 100.0).round() as i64);
    Ok(())
}

#[tauri::command]
fn get_overlay_opacity(state: State<AppState>) -> f64 {
    state.config.lock().unwrap().overlay_opacity
}

#[tauri::command]
fn get_overlay_layout(state: State<AppState>) -> String {
    state.config.lock().unwrap().overlay_layout.clone()
}

#[tauri::command]
fn set_overlay_layout(
    app: AppHandle,
    state: State<AppState>,
    layout: String,
) -> Result<(), String> {
    if layout != "h" && layout != "v" {
        return Err("invalid overlay layout".into());
    }
    state.update_config(|cfg| cfg.overlay_layout = layout.clone())?;
    let _ = app.emit("overlay-layout", layout);
    Ok(())
}

#[tauri::command]
fn move_overlay_preset(app: AppHandle, state: State<AppState>, pos: String) -> Result<(), String> {
    let w = overlay_window(&app)?;
    if !matches!(pos.as_str(), "tl" | "tc" | "tr" | "bl" | "bc" | "br") {
        return Err("Unknown overlay position".into());
    }
    let size = w.outer_size().map_err(|e| e.to_string())?;
    let origin = w.outer_position().map_err(|e| e.to_string())?;
    let anchor = Some(OverlayPos {
        x: origin.x as f64,
        y: origin.y as f64,
    });
    place_overlay(
        &w,
        Some(&pos),
        anchor,
        size.width as i32,
        size.height as i32,
    )?;
    let origin = w.outer_position().map_err(|e| e.to_string())?;
    state.update_config(|cfg| {
        cfg.overlay_preset = Some(pos);
        cfg.overlay_pos = Some(OverlayPos {
            x: origin.x as f64,
            y: origin.y as f64,
        });
    })
}

fn main() {
    tauri::Builder::default()
        .manage(sharing::Sharing::default())
        .on_window_event(|window, event| {
            if window.label() == "main" && matches!(event, tauri::WindowEvent::Destroyed) {
                window.app_handle().exit(0);
            }
        })
        .invoke_handler(tauri::generate_handler![
            sharing::get_sharing,
            sharing::start_sharing,
            sharing::stop_sharing,
            sensors::sensor_driver_installed,
            sensors::enable_sensor_driver,
            get_settings,
            save_settings,
            set_always_on_top,
            show_overlay,
            set_overlay_game_only,
            resize_overlay,
            overlay_lock,
            overlay_drag_start,
            overlay_drag_end,
            move_overlay_preset,
            set_overlay_opacity,
            get_overlay_opacity,
            get_overlay_layout,
            set_overlay_layout
        ])
        .setup(|app| {
            tray::setup(app.handle());
            let dir = app.path().app_config_dir()?;
            std::fs::create_dir_all(&dir).ok();
            let config_path = dir.join("config.json");
            let mut config = std::fs::read_to_string(&config_path)
                .ok()
                .and_then(|s| serde_json::from_str::<Config>(&s).ok())
                .unwrap_or_default();
            config.move_cpu_settings();
            if !config_path.exists() {
                std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).ok();
            }
            let resource_dir = app.path().resource_dir()?;
            let state = AppState {
                config: Arc::new(Mutex::new(config)),
                cards: Arc::new(Mutex::new(HashMap::new())),
                config_path,
                overlay_locked: AtomicBool::new(true),
                game_in_front: AtomicBool::new(false),
                capture: Mutex::new(Some(frame_capture::FrameCapture::new(&resource_dir))),
            };
            app.manage(state);
            let st = app.state::<AppState>();
            reconcile_overlay(app.handle(), &st)?;
            let libre_cards = st.cards.clone();
            thread::spawn(move || {
                sensors::run(resource_dir, move |id, vendor, name, metrics| {
                    record(&libre_cards, id, vendor, name, metrics, Source::Libre);
                })
            });
            let amd_cards = st.cards.clone();
            thread::spawn(move || adlx::run(amd_cards));
            let nv_cards = st.cards.clone();
            thread::spawn(move || nvidia_fallback(nv_cards));
            let (c2, k2) = (st.config.clone(), st.cards.clone());
            let handle = app.handle().clone();
            thread::spawn(move || main_loop(handle, c2, k2));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                if let Some(state) = app.try_state::<AppState>() {
                    state.capture.lock().unwrap().take();
                }
            }
        });
}
