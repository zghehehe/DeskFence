$ErrorActionPreference='Continue'
Add-Type @"
using System; using System.Text; using System.Runtime.InteropServices;
public class Q1 { public delegate bool EP(IntPtr h, IntPtr l);
[DllImport("user32.dll")] public static extern bool EnumWindows(EP p, IntPtr l);
[DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h); }
"@
$c=New-Object System.Collections.ArrayList
$cb=[Q1+EP]{ param($h,$l)
    $sb=New-Object System.Text.StringBuilder 64
    [void][Q1]::GetClassName($h,$sb,64)
    if($sb.ToString().StartsWith('DeskFence')){ [void]$c.Add(($sb.ToString()+" vis="+[Q1]::IsWindowVisible($h))) }
    return $true }
[void][Q1]::EnumWindows($cb,[IntPtr]::Zero)
$c | Group-Object | ForEach-Object { "{0}x {1}" -f $_.Count,$_.Name }
