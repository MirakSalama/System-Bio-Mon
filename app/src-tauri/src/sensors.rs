use serde::Deserialize;
use serde_json::Number;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

const CREATE_NO_WINDOW: u32 = 0x08000000;
static INSTALLING: AtomicBool = AtomicBool::new(false);

#[derive(Deserialize)]
struct Reading {
    id: String,
    name: String,
    vendor: String,
    metrics: BTreeMap<String, Number>,
}

#[derive(Deserialize)]
struct Snapshot {
    cards: Vec<Reading>,
}

pub fn run(
    resource_dir: PathBuf,
    publish: impl Fn(String, String, String, BTreeMap<String, Number>),
) {
    let helper = resource_dir
        .join("resources")
        .join("sensors")
        .join("HardwareSensors.exe");
    let mut first = true;
    loop {
        let child = Command::new(&helper)
            .args([
                if first { "--monitor" } else { "--basic" },
                &std::process::id().to_string(),
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = child {
            first = false;
            if let Some(stdout) = child.stdout.take() {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if let Ok(snapshot) = serde_json::from_str::<Snapshot>(&line) {
                        for c in snapshot.cards {
                            if !c.metrics.is_empty() {
                                publish(c.id, c.vendor, c.name, c.metrics);
                            }
                        }
                    }
                }
            }
            let _ = child.wait();
        }
        thread::sleep(Duration::from_secs(5));
    }
}

fn system_program(name: &str) -> PathBuf {
    // Use the mapped Windows system directory even when a caller customizes PATH.
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join(name)
}

#[tauri::command]
pub async fn sensor_driver_installed() -> bool {
    tauri::async_runtime::spawn_blocking(|| {
        Command::new(system_program("reg.exe"))
            .args(["query", r"HKLM\SYSTEM\CurrentControlSet\Services\PawnIO"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .is_ok_and(|result| result.status.success())
    })
    .await
    .unwrap_or(false)
}

#[tauri::command]
pub async fn enable_sensor_driver() -> Result<(), String> {
    if INSTALLING.swap(true, Ordering::AcqRel) {
        return Err("Driver setup is already open.".into());
    }
    let result = tauri::async_runtime::spawn_blocking(|| {
        let out = Command::new(system_program(r"WindowsPowerShell\v1.0\powershell.exe"))
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                include_str!("../../sensors/setup-driver.ps1"),
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr)
                .trim()
                .chars()
                .take(500)
                .collect::<String>())
        }
    })
    .await;
    INSTALLING.store(false, Ordering::Release);
    result.map_err(|e| e.to_string())?
}
