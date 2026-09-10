# Sink-block watcher: high-freq sampling of windows immediately above/below
# the desktop host. ASCII only. Usage: sinkwatch.ps1 <durationMs> <outFile>
param([int]$DurationMs = 10000, [string]$OutFile = "$env:TEMP\sinkwatch.txt")
$ErrorActionPreference = 'Stop'
Add-Type @"
using System;
using System.Text;
using System.IO;
using System.Threading;
using System.Diagnostics;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class SW {
    [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr h, uint cmd);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
    static Dictionary<long, string> clsCache = new Dictionary<long, string>();
    static Dictionary<long, uint> tidCache = new Dictionary<long, uint>();
    static string Cls(IntPtr h) {
        string s;
        if (clsCache.TryGetValue(h.ToInt64(), out s)) return s;
        StringBuilder sb = new StringBuilder(64);
        GetClassName(h, sb, 64);
        s = sb.ToString();
        if (clsCache.Count < 4096) clsCache[h.ToInt64()] = s;
        return s;
    }
    static uint Tid(IntPtr h) {
        uint t;
        if (tidCache.TryGetValue(h.ToInt64(), out t)) return t;
        uint pid;
        t = GetWindowThreadProcessId(h, out pid);
        if (tidCache.Count < 4096) tidCache[h.ToInt64()] = t;
        return t;
    }
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowW(string cls, string cap);
    public static IntPtr FindProgman() { return FindWindowW("Progman", null); }
    public static void Watch(IntPtr host, int durationMs, int intervalMs, string path) {
        SetProcessDpiAwarenessContext((IntPtr)(-4));
        using (StreamWriter w = new StreamWriter(path, false)) {
            Stopwatch sw = Stopwatch.StartNew();
            while (sw.ElapsedMilliseconds < (long)durationMs) {
                StringBuilder sb = new StringBuilder(4096);
                sb.Append('T').Append(sw.ElapsedMilliseconds);
                IntPtr p = GetWindow(host, 3); // GW_HWNDPREV = above host
                for (int i = 0; i < 45 && p != IntPtr.Zero; i++) {
                    sb.Append("|A").Append(i).Append(',').Append(p.ToInt64().ToString("X"))
                      .Append(',').Append(Cls(p)).Append(',').Append(Tid(p));
                    p = GetWindow(p, 3);
                }
                p = GetWindow(host, 2); // GW_HWNDNEXT = below host
                for (int i = 0; i < 25 && p != IntPtr.Zero; i++) {
                    sb.Append("|B").Append(i).Append(',').Append(p.ToInt64().ToString("X"))
                      .Append(',').Append(Cls(p)).Append(',').Append(Tid(p));
                    p = GetWindow(p, 2);
                }
                w.WriteLine(sb.ToString());
                Thread.Sleep(intervalMs);
            }
        }
    }
}
"@
$host0 = [SW]::FindProgman()
if ($host0 -eq [IntPtr]::Zero) { Write-Output "no Progman"; exit 1 }
Write-Output ("host=0x{0:X} dur={1}ms out={2}" -f $host0.ToInt64(), $DurationMs, $OutFile)
[SW]::Watch($host0, $DurationMs, 12, $OutFile)
Write-Output "done"
