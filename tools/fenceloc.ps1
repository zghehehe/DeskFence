# Locate every DeskFenceFence window in the full z-stack: index from top,
# visibility, rect - plus host position. ASCII only.
$ErrorActionPreference = 'Continue'
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class FL {
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  public delegate bool CB(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(CB cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  public static void Dump() {
    StringBuilder sb = new StringBuilder(64);
    var wins = new System.Collections.Generic.List<IntPtr>();
    EnumWindows(delegate(IntPtr h, IntPtr l) { wins.Add(h); return true; }, IntPtr.Zero);
    int total = wins.Count;
    int progmanIdx = -1;
    for (int i = 0; i < total; i++) {
      GetClassName(wins[i], sb, 64);
      if (sb.ToString() == "Progman") { progmanIdx = i; break; }
    }
    Console.WriteLine("total=" + total + " progmanIdxFromTop=" + progmanIdx);
    for (int i = 0; i < total; i++) {
      GetClassName(wins[i], sb, 64);
      string c = sb.ToString();
      if (c == "DeskFenceFence" || c == "Progman") {
        RECT r; GetWindowRect(wins[i], out r);
        string loc = (i < progmanIdx) ? ("aboveHost+" + (progmanIdx - i)) : "BELOW-HOST";
        Console.WriteLine("idxFromTop=" + i + " " + c + " rect=(" + r.L + "," + r.T + ")-(" + r.R + "," + r.B + ") vis=" + IsWindowVisible(wins[i]) + " " + loc);
      }
    }
  }
}
"@
[void][FL]::SetProcessDpiAwarenessContext([IntPtr](-4))
[FL]::Dump()
