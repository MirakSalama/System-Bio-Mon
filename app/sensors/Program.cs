using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.IO.Pipes;
using System.Runtime.InteropServices;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Text;
using Microsoft.Win32;
using Microsoft.Win32.SafeHandles;

internal static class Program
{
    private static Process parent;
    private static readonly Encoding Utf8 = new UTF8Encoding(false);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetNamedPipeClientProcessId(SafePipeHandle pipe, out uint pid);

    private static void WatchParent(string id)
    {
        parent = Process.GetProcessById(Int32.Parse(id, CultureInfo.InvariantCulture));
        parent.Exited += delegate { Environment.Exit(0); };
        parent.EnableRaisingEvents = true;
        if (parent.HasExited) Environment.Exit(0);
    }

    private static bool IsAdmin()
    {
        using (var identity = WindowsIdentity.GetCurrent())
            return new WindowsPrincipal(identity).IsInRole(WindowsBuiltInRole.Administrator);
    }

    // This pipe carries readings only. The privileged client never accepts commands.
    private static void TryElevated()
    {
        string pipe = "GpuMonitor-" + Guid.NewGuid().ToString("N");
        var security = new PipeSecurity();
        using (var identity = WindowsIdentity.GetCurrent())
            security.AddAccessRule(new PipeAccessRule(identity.User, PipeAccessRights.FullControl, AccessControlType.Allow));
        var admins = new SecurityIdentifier(WellKnownSidType.BuiltinAdministratorsSid, null);
        security.AddAccessRule(new PipeAccessRule(admins, PipeAccessRights.Write, AccessControlType.Allow));
        using (var server = new NamedPipeServerStream(pipe, PipeDirection.In, 1, PipeTransmissionMode.Byte,
            PipeOptions.Asynchronous, 4096, 4096, security))
        {
            var start = new ProcessStartInfo(Process.GetCurrentProcess().MainModule.FileName,
                "--worker " + pipe + " " + Process.GetCurrentProcess().Id.ToString(CultureInfo.InvariantCulture))
            {
                UseShellExecute = true, Verb = "runas", WindowStyle = ProcessWindowStyle.Hidden,
                WorkingDirectory = AppDomain.CurrentDomain.BaseDirectory
            };
            using (var worker = Process.Start(start))
            {
                var connect = server.BeginWaitForConnection(null, null);
                if (!connect.AsyncWaitHandle.WaitOne(10000)) return;
                server.EndWaitForConnection(connect);
                uint client;
                if (!GetNamedPipeClientProcessId(server.SafePipeHandle, out client) || client != worker.Id) return;
                using (var reader = new StreamReader(server, Utf8))
                {
                    string line;
                    while ((line = reader.ReadLine()) != null) Console.WriteLine(line);
                }
                // A worker that terminates unexpectedly leaves basic readings available.
            }
        }
    }

    public static int Main(string[] args)
    {
        try
        {
            Console.OutputEncoding = Utf8;
            bool installed;
            using (var key = Registry.LocalMachine.OpenSubKey(@"SYSTEM\CurrentControlSet\Services\PawnIO"))
                installed = key != null;
            if (args.Length == 3 && args[0] == "--worker")
            {
                Guid guid;
                if (!args[1].StartsWith("GpuMonitor-", StringComparison.Ordinal) ||
                    !Guid.TryParseExact(args[1].Substring(11), "N", out guid)) return 2;
                WatchParent(args[2]);
                using (var pipe = new NamedPipeClientStream(".", args[1], PipeDirection.Out,
                    PipeOptions.None, TokenImpersonationLevel.Anonymous))
                {
                    pipe.Connect(10000);
                    using (var writer = new StreamWriter(pipe, Utf8)) SensorReader.Run(writer, false, installed && IsAdmin());
                }
                return 0;
            }
            bool once = args.Length == 1 && args[0] == "--once";
            if (!once)
            {
                if (args.Length != 2 || (args[0] != "--monitor" && args[0] != "--basic")) return 2;
                WatchParent(args[1]);
                if (args[0] == "--monitor" && installed && !IsAdmin())
                {
                    try { TryElevated(); }
                    catch (Win32Exception ex) { Console.Error.WriteLine(ex.Message); }
                    catch (IOException ex) { Console.Error.WriteLine(ex.Message); }
                }
            }
            SensorReader.Run(Console.Out, once, installed && IsAdmin());
            return 0;
        }
        catch (Exception ex) { Console.Error.WriteLine(ex.Message); return 1; }
    }
}
