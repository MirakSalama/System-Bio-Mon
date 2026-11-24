using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Threading;
using System.Web.Script.Serialization;
using LibreHardwareMonitor.Hardware;

internal static class SensorReader
{
    private sealed class Card
    {
        public string id { get; set; }
        public string name { get; set; }
        public string vendor { get; set; }
        public Dictionary<string, double> metrics { get; set; }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct PowerStatus
    {
        public byte Ac, Flags, Charge, Saver;
        public int Remaining, Full;
    }

    [DllImport("kernel32.dll")]
    private static extern bool GetSystemPowerStatus(out PowerStatus status);

    private static void Put(Dictionary<string, double> m, string key, double? value)
    {
        if (value.HasValue && !Double.IsNaN(value.Value) && !Double.IsInfinity(value.Value))
            m[key] = Math.Round(value.Value, 1);
    }

    private static double? Read(IHardware hw, SensorType type, params string[] names)
    {
        foreach (string name in names)
        {
            bool capacity = type == SensorType.SmallData && name.EndsWith(" Total", StringComparison.Ordinal);
            ISensor sensor = hw.Sensors.FirstOrDefault(s => s.SensorType == type &&
                String.Equals(s.Name, name, StringComparison.OrdinalIgnoreCase) &&
                s.Value.HasValue && (capacity || s.Max.HasValue));
            if (sensor != null) return sensor.Value;
        }
        return null;
    }

    private static Dictionary<string, double> Gpu(IHardware hw)
    {
        var m = new Dictionary<string, double>();
        var hardwareLoad = Read(hw, SensorType.Load, "GPU Core", "GPU D3D");
        Put(m, "usage", hardwareLoad ?? hw.Sensors.Where(s => s.SensorType == SensorType.Load &&
            s.Name.StartsWith("D3D ") && s.Max.HasValue).Select(s => (double?)s.Value).Max());
        Put(m, "temperature", Read(hw, SensorType.Temperature, "GPU Core", "GPU"));
        Put(m, "hotspot", Read(hw, SensorType.Temperature, "GPU Hot Spot", "GPU Hotspot"));
        Put(m, "clock", Read(hw, SensorType.Clock, "GPU Core"));
        Put(m, "memClock", Read(hw, SensorType.Clock, "GPU Memory"));
        Put(m, "power", Read(hw, SensorType.Power, "GPU Package", "GPU PPT", "GPU Core"));
        Put(m, "voltage", Read(hw, SensorType.Voltage, "GPU Core Voltage", "GPU Core") * 1000);
        Put(m, "fan", Read(hw, SensorType.Fan, "GPU Fan", "GPU Fan 1", "GPU Fan 0"));
        Put(m, "vramUsed", Read(hw, SensorType.SmallData, "GPU Memory Used", "D3D Dedicated Memory Used"));
        Put(m, "vramTotal", Read(hw, SensorType.SmallData, "GPU Memory Total", "D3D Dedicated Memory Total"));
        Put(m, "sharedUsed", Read(hw, SensorType.SmallData, "D3D Shared Memory Used"));
        Put(m, "sharedTotal", Read(hw, SensorType.SmallData, "D3D Shared Memory Total"));
        return m;
    }

    private static void ReadHardware(IHardware hw, List<Card> cards, bool privileged)
    {
        // Several vendor backends retain Value when a driver read fails.
        // LHM only sets Max on a successful value assignment in this poll.
        foreach (ISensor sensor in hw.Sensors)
        {
            sensor.ResetMax();
            sensor.ValuesTimeWindow = TimeSpan.Zero;
        }
        hw.Update();
        string type = hw.HardwareType.ToString();
        string index = hw.Identifier.ToString().Split('/').Last();
        var m = new Dictionary<string, double>();
        string vendor = "system", id = null;
        if (type == "GpuNvidia" || type == "GpuAmd" || type == "GpuIntel")
        {
            vendor = type.Substring(3).ToLowerInvariant();
            id = (vendor == "nvidia" ? "nv" : vendor) + "-" + index;
            m = Gpu(hw);
        }
        else if (type == "Cpu" && privileged)
        {
            id = "cpu-" + index;
            Put(m, "cpuTemp", Read(hw, SensorType.Temperature, "Core (Tctl/Tdie)", "CPU Package", "CPU (Tctl)", "Core Max"));
            Put(m, "cpuPower", Read(hw, SensorType.Power, "Package", "CPU Package"));
            Put(m, "cpuClock", Read(hw, SensorType.Clock, "Cores (Average)", "CPU Core #1"));
            foreach (string key in m.Where(pair => pair.Value <= 0).Select(pair => pair.Key).ToArray()) m.Remove(key);
        }
        else if (type == "Battery")
        {
            id = "battery-" + index;
            Put(m, "batteryCharge", Read(hw, SensorType.Level, "Charge Level"));
            Put(m, "chargePower", Read(hw, SensorType.Power, "Charge Rate"));
            Put(m, "dischargePower", Read(hw, SensorType.Power, "Discharge Rate"));
        }
        else if (type == "SuperIO" || type == "EmbeddedController" || type == "Motherboard")
        {
            foreach (ISensor s in hw.Sensors.Where(s => s.Value.HasValue && s.Max.HasValue &&
                (s.SensorType == SensorType.Fan || s.SensorType == SensorType.Temperature)))
            {
                var board = new Dictionary<string, double>();
                Put(board, s.SensorType == SensorType.Fan ? "fan" : "boardTemp", s.Value);
                cards.Add(new Card { id = "board-" + s.Identifier.ToString().Trim('/').Replace('/', '-'),
                    name = s.Name, vendor = vendor, metrics = board });
            }
        }
        if (id != null && m.Keys.Any(k => k != "vramTotal" && k != "sharedTotal"))
            cards.Add(new Card { id = id, name = hw.Name, vendor = vendor, metrics = m });
        foreach (IHardware sub in hw.SubHardware) ReadHardware(sub, cards, privileged);
    }

    public static void Run(TextWriter output, bool once, bool privileged)
    {
        var json = new JavaScriptSerializer();
        var pc = new Computer { IsGpuEnabled = true, IsCpuEnabled = privileged,
            IsMotherboardEnabled = privileged, IsBatteryEnabled = true };
        try
        {
            pc.Open();
            do
            {
                var cards = new List<Card>();
                foreach (IHardware hw in pc.Hardware)
                {
                    try { ReadHardware(hw, cards, privileged); }
                    catch (Exception ex) { Console.Error.WriteLine(hw.Name + ": " + ex.Message); }
                }
                PowerStatus battery;
                if (!cards.Any(c => c.id.StartsWith("battery-")) && GetSystemPowerStatus(out battery) &&
                    battery.Charge <= 100 && battery.Flags != 128 && battery.Flags != 255)
                    cards.Add(new Card { id = "battery-0", name = "Battery", vendor = "system",
                        metrics = new Dictionary<string, double> { { "batteryCharge", battery.Charge } } });
                output.WriteLine(json.Serialize(new { cards = cards }));
                output.Flush();
                if (!once) Thread.Sleep(1000);
            } while (!once);
        }
        finally { pc.Close(); }
    }
}
