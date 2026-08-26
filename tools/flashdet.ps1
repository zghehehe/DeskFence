param(
    [int]$Seconds = 6,
    [string]$Prefix = "C:\Users\z00897910\Desktop\DeskFence\target\fd"
)
$ErrorActionPreference = 'Stop'
$src = @"
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;

public static class FD {
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr hWnd, IntPtr hDC);
    [DllImport("gdi32.dll")] public static extern IntPtr CreateCompatibleDC(IntPtr hdc);
    [DllImport("gdi32.dll")] public static extern IntPtr CreateDIBSection(IntPtr hdc, ref BI bmi, uint usage, out IntPtr bits, IntPtr hSection, uint offset);
    [DllImport("gdi32.dll")] public static extern bool DeleteDC(IntPtr hdc);
    [DllImport("gdi32.dll")] public static extern IntPtr SelectObject(IntPtr hdc, IntPtr hgdiobj);
    [DllImport("gdi32.dll")] public static extern bool DeleteObject(IntPtr h);
    [DllImport("gdi32.dll")] public static extern bool BitBlt(IntPtr hdc, int x, int y, int w, int h, IntPtr src, int x1, int y1, int rop);

    [StructLayout(LayoutKind.Sequential)]
    public struct BH { public uint Size; public int W; public int H; public ushort P, B; public uint C, SI; public int XP, YP; public uint CU, CI; }
    [StructLayout(LayoutKind.Sequential)]
    public struct BI { public BH H; public uint Clr; }

    static IntPtr MakeDib(IntPtr hdc, int w, int h, out IntPtr bits) {
        BI bmi = new BI();
        bmi.H.Size = (uint)Marshal.SizeOf(typeof(BH));
        bmi.H.W = w; bmi.H.H = -h; bmi.H.P = 1; bmi.H.B = 32; bmi.H.C = 0;
        return CreateDIBSection(hdc, ref bmi, 0, out bits, IntPtr.Zero, 0);
    }

    static long CountDiff(byte[] a, byte[] b) {
        long c = 0;
        for (int i = 0; i < a.Length; i += 16) {
            if (a[i] != b[i] || a[i+4] != b[i+4] || a[i+8] != b[i+8]) c++;
        }
        return c;
    }

    public static void Run(int seconds, string prefix) {
        SetProcessDPIAware();
        Rectangle sb = System.Windows.Forms.Screen.PrimaryScreen.Bounds;
        int W = sb.Width, H = sb.Height;
        int sz = W * H * 4;
        IntPtr scr = GetDC(IntPtr.Zero);
        IntPtr mdc = CreateCompatibleDC(scr);
        IntPtr b0, b1;
        IntPtr d0 = MakeDib(mdc, W, H, out b0);
        IntPtr d1 = MakeDib(mdc, W, H, out b1);
        IntPtr old = SelectObject(mdc, d0);
        byte[] bufA = new byte[sz];
        byte[] bufB = new byte[sz];
        long frames = 0, events = 0;
        bool cur0 = true;
        double prevMs = 0;
        var sw = System.Diagnostics.Stopwatch.StartNew();
        while (sw.Elapsed.TotalSeconds < seconds) {
            IntPtr dst = cur0 ? d0 : d1;
            IntPtr bits = cur0 ? b0 : b1;
            IntPtr bitsPrev = cur0 ? b1 : b0;
            byte[] cur = cur0 ? bufA : bufB;
            byte[] prev = cur0 ? bufB : bufA;
            SelectObject(mdc, dst);
            BitBlt(mdc, 0, 0, W, H, scr, sb.X, sb.Y, 0x00CC0020);
            frames++;
            if (frames > 1) {
                Marshal.Copy(bits, cur, 0, sz);
                long d = CountDiff(cur, prev);
                if (d > 5000) {
                    events++;
                    double ms = sw.Elapsed.TotalMilliseconds;
                    Console.WriteLine(string.Format("EVENT {0}: t={1:F0}ms diff_px={2} span={3:F0}ms", events, ms, d, ms - prevMs));
                    if (events <= 8) {
                        Bitmap bmpA = new Bitmap(W, H, W * 4, PixelFormat.Format32bppRgb, bitsPrev);
                        Bitmap bmpB = new Bitmap(W, H, W * 4, PixelFormat.Format32bppRgb, bits);
                        bmpA.Save(prefix + "_ev" + events + "_a.png", ImageFormat.Png);
                        bmpB.Save(prefix + "_ev" + events + "_b.png", ImageFormat.Png);
                        bmpA.Dispose(); bmpB.Dispose();
                    }
                    prevMs = ms;
                }
            }
            cur0 = !cur0;
        }
        SelectObject(mdc, old);
        DeleteObject(d0); DeleteObject(d1); DeleteDC(mdc);
        ReleaseDC(IntPtr.Zero, scr);
        Console.WriteLine(string.Format("DONE frames={0} fps={1:F0} events={2}", frames, frames / (double)seconds, events));
    }
}
"@
Add-Type -TypeDefinition $src -ReferencedAssemblies @('System.Drawing','System.Windows.Forms') -PassThru | Out-Null
[FD]::Run($Seconds, $Prefix)
