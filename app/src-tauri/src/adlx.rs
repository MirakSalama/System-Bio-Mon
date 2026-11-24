use super::{record, Cards, Metrics, Source};
use amd_adlx::*;
use serde_json::Number;
use std::{collections::HashMap, ffi::CStr, ptr, thread, time::Duration};

// ADLX owns the vtables and returns one reference per successful interface getter.
// Keep all interfaces on this worker and release them before the runtime unloads.
pub fn run(cards: Cards) {
    let mut stamps = HashMap::new();
    loop {
        if let Ok(runtime) = AdlxHelper::new() {
            while sample(&runtime, &cards, &mut stamps) {
                thread::sleep(Duration::from_secs(1));
            }
        }
        thread::sleep(Duration::from_secs(15));
    }
}

fn sample(runtime: &AdlxHelper, cards: &Cards, stamps: &mut HashMap<u32, i64>) -> bool {
    unsafe {
        let sys = runtime.system_services();
        let api = &*(*sys).pVtbl;
        let mut perf = ptr::null_mut();
        if !adlx_succeeded((api.GetPerformanceMonitoringServices)(sys, &mut perf)) {
            return false;
        }
        let mut list = ptr::null_mut();
        let mut found = false;
        if adlx_succeeded((api.GetGPUs)(sys, &mut list)) {
            let api = &*(*list).pVtbl;
            for idx in (api.Begin)(list)..(api.End)(list) {
                let mut gpu = ptr::null_mut();
                if adlx_succeeded((api.At_GPUList)(list, idx, &mut gpu)) {
                    if let Some((name, readings, stamp)) = read_gpu(perf, gpu) {
                        found = true;
                        if stamp.is_none_or(|t| stamps.insert(idx, t) != Some(t)) {
                            record(
                                cards,
                                format!("amd-{idx}"),
                                "amd".into(),
                                name,
                                readings,
                                Source::Adlx,
                            );
                        }
                    }
                    ((*(*gpu).pVtbl).Release)(gpu);
                }
            }
            (api.Release)(list);
        }
        ((*(*perf).pVtbl).Release)(perf);
        found
    }
}

unsafe fn read_gpu(
    perf: *mut IADLXPerformanceMonitoringServices,
    gpu: *mut IADLXGPU,
) -> Option<(String, Metrics, Option<i64>)> {
    let api = &*(*perf).pVtbl;
    let mut support = ptr::null_mut();
    if !adlx_succeeded((api.GetSupportedGPUMetrics)(perf, gpu, &mut support)) {
        return None;
    }
    let mut data = ptr::null_mut();
    let mut readings = Metrics::new();
    let mut stamp = None;
    if adlx_succeeded((api.GetCurrentGPUMetrics)(perf, gpu, &mut data)) {
        let capabilities = &*(*support).pVtbl;
        let metrics = &*(*data).pVtbl;
        let mut tick = 0;
        if adlx_succeeded((metrics.TimeStamp)(data, &mut tick)) && tick > 0 {
            stamp = Some(tick);
        }
        macro_rules! read_supported {
            ($key:literal, $support:ident, $get:ident) => {{
                let mut yes = 0;
                let mut value = Default::default();
                if adlx_succeeded((capabilities.$support)(support, &mut yes))
                    && yes != 0
                    && adlx_succeeded((metrics.$get)(data, &mut value))
                {
                    if let Some(value) = Number::from_f64(((value as f64) * 10.0).round() / 10.0) {
                        readings.insert($key.into(), value);
                    }
                }
            }};
        }
        read_supported!("usage", IsSupportedGPUUsage, GPUUsage);
        read_supported!("temperature", IsSupportedGPUTemperature, GPUTemperature);
        read_supported!(
            "hotspot",
            IsSupportedGPUHotspotTemperature,
            GPUHotspotTemperature
        );
        read_supported!("clock", IsSupportedGPUClockSpeed, GPUClockSpeed);
        read_supported!("memClock", IsSupportedGPUVRAMClockSpeed, GPUVRAMClockSpeed);
        read_supported!("vramUsed", IsSupportedGPUVRAM, GPUVRAM);
        read_supported!("power", IsSupportedGPUPower, GPUPower);
        read_supported!(
            "boardPower",
            IsSupportedGPUTotalBoardPower,
            GPUTotalBoardPower
        );
        read_supported!("voltage", IsSupportedGPUVoltage, GPUVoltage);
        read_supported!("fan", IsSupportedGPUFanSpeed, GPUFanSpeed);
        (metrics.Release)(data);
    }
    ((*(*support).pVtbl).Release)(support);
    if readings.is_empty() {
        return None;
    }
    let gpu_api = &*(*gpu).pVtbl;
    let mut total = 0;
    if adlx_succeeded((gpu_api.TotalVRAM)(gpu, &mut total)) && total > 0 {
        readings.insert("vramTotal".into(), total.into());
    }
    let mut name = ptr::null();
    let name = if adlx_succeeded((gpu_api.Name)(gpu, &mut name)) && !name.is_null() {
        CStr::from_ptr(name).to_string_lossy().into_owned()
    } else {
        "AMD GPU".into()
    };
    Some((name, readings, stamp))
}

#[test]
#[ignore = "requires a supported physical AMD GPU"]
fn reads_installed_radeon() {
    let runtime = AdlxHelper::new().expect("AMD driver");
    let cards = Cards::default();
    let mut stamps = HashMap::new();
    assert!(sample(&runtime, &cards, &mut stamps));
    for card in cards.lock().unwrap().values() {
        println!("{} {:?}", card.name, card.metrics);
        assert!(card.metrics.contains_key("usage") || card.metrics.contains_key("temperature"));
    }
}
