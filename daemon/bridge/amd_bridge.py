# Optional legacy AMD reader. The desktop app uses the bundled native ADLX reader.
import argparse
import gc
import json
import sys
import time

try:
    from adlx import ADLX
except ImportError as exc:
    print(f"ADLX Python reader unavailable: {exc}", file=sys.stderr)
    sys.exit(78)

# (metricId, supportFlag, getter)
GPU_METRICS = [
    ("usage",        "IsSupportedGPUUsage",              "GPUUsage"),
    ("clock",        "IsSupportedGPUClockSpeed",         "GPUClockSpeed"),
    ("memClock",     "IsSupportedGPUVRAMClockSpeed",     "GPUVRAMClockSpeed"),
    ("vramUsed",     "IsSupportedGPUVRAM",               "GPUVRAM"),
    ("temperature",  "IsSupportedGPUTemperature",        "GPUTemperature"),
    ("hotspot",      "IsSupportedGPUHotspotTemperature", "GPUHotspotTemperature"),
    ("power",        "IsSupportedGPUPower",              "GPUPower"),
    ("boardPower",   "IsSupportedGPUTotalBoardPower",    "GPUTotalBoardPower"),
    ("voltage",      "IsSupportedGPUVoltage",            "GPUVoltage"),
    ("fan",          "IsSupportedGPUFanSpeed",           "GPUFanSpeed"),
]

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--interval", type=float, default=1.0)
    ap.add_argument("--once", action="store_true")
    args = ap.parse_args()

    helper = ADLX.ADLXHelper()
    if helper.Initialize() != ADLX.ADLX_RESULT.ADLX_OK:
        raise SystemExit("Could not initialize the installed AMD driver.")
    failure = None
    try:
        sample(helper, args)
    except (KeyboardInterrupt, BrokenPipeError):
        pass
    except Exception as exc:
        # Preserve the diagnostic without keeping live services in an exception traceback.
        failure = str(exc)
    # All pybind sensor/service references must be released before ADLX shuts down.
    gc.collect()
    helper.Terminate()
    if failure is not None:
        raise SystemExit(failure)

def sample(helper, args):
    system = helper.GetSystemServices()
    perf = system.GetPerformanceMonitoringServices()
    gpus = [g for g in system.GetGPUs() if g.Type() != ADLX.ADLX_GPU_TYPE.GPUTYPE_UNDEFINED]
    if args.once:
        emit_frame(gpus, perf)
        return

    inventory = [{"id": f"amd-{i}", "index": i, "name": str(g.Name())} for i, g in enumerate(gpus)]
    print(json.dumps({"type": "hello", "gpus": inventory}), flush=True)
    while True:
        emit_frame(gpus, perf)
        time.sleep(args.interval)

def emit_frame(gpus, perf):
    try:
        for i, gpu in enumerate(gpus):
            metrics = {}
            try:
                support = perf.GetSupportedGPUMetrics(gpu)
                gm = perf.GetCurrentGPUMetrics(gpu)
                for mid, sup, getter in GPU_METRICS:
                    try:
                        if support and getattr(support, sup)():
                            val = getattr(gm, getter)()
                            if val is not None:
                                metrics[mid] = float(val)
                    except Exception:
                        pass
            except Exception:
                pass

            system = {}
            try:
                sysup = perf.GetSupportedSystemMetrics()
                sm = perf.GetCurrentSystemMetrics()
                if sysup.IsSupportedCPUUsage():
                    system["cpu"] = float(sm.CPUUsage())
                if sysup.IsSupportedSystemRAM():
                    system["ram"] = float(sm.SystemRAM())
            except Exception:
                pass

            print(json.dumps({
                "type": "frame",
                "id": f"amd-{i}",
                "name": str(gpu.Name()),
                "metrics": metrics,
                "system": system,
            }), flush=True)
    except Exception as e:
        print(json.dumps({"type": "error", "message": str(e)}), flush=True)

if __name__ == "__main__":
    main()
