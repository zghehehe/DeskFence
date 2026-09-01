# Read-only: query token elevation of a process via limited query rights.
param([int]$Pid2)
$ErrorActionPreference='Continue'
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class WP2 {
[DllImport("kernel32.dll")] public static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
[DllImport("advapi32.dll")] public static extern bool OpenProcessToken(IntPtr p, uint access, out IntPtr tok);
[DllImport("advapi32.dll")] public static extern bool GetTokenInformation(IntPtr tok, int cls, out int info, int len, out int retLen);
[DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
}
"@
$h = [WP2]::OpenProcess(0x1000, $false, $Pid2)  # PROCESS_QUERY_LIMITED_INFORMATION
if ($h -eq [IntPtr]::Zero) {
  "OpenProcess(LIMITED) failed err=$([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
  exit
}
$tok=[IntPtr]::Zero
if (-not [WP2]::OpenProcessToken($h, 0x0008, [ref]$tok)) {
  "OpenProcessToken failed err=$([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
} else {
  $elev=0; $rl=0
  if ([WP2]::GetTokenInformation($tok, 20, [ref]$elev, 4, [ref]$rl)) {
    "elevated=$([bool]$elev)"
  } else { "GetTokenInformation failed err=$([Runtime.InteropServices.Marshal]::GetLastWin32Error())" }
  [void][WP2]::CloseHandle($tok)
}
[void][WP2]::CloseHandle($h)
