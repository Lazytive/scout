param([long]$Hwnd, [string]$Out)
# Usage: powershell -File tools/window-shot.ps1 -Hwnd <HWND> -Out shot.png
# (find the HWND with `klip-cli windows Scout`). For UI checks run the app with
# KLIP_WINDOW_POS off-screen (e.g. 12000,200): nothing shows on anyone's monitors.
# Capture one window's own content (PrintWindow with PW_RENDERFULLCONTENT), even when
# other windows cover it; nothing else on screen ends up in the picture.
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class W {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
}
"@
[W]::SetProcessDPIAware() | Out-Null
$h = [IntPtr]$Hwnd
$r = New-Object W+RECT
[W]::GetWindowRect($h, [ref]$r) | Out-Null
$w = $r.R - $r.L; $ht = $r.B - $r.T
$bmp = New-Object System.Drawing.Bitmap $w, $ht
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
$ok = [W]::PrintWindow($h, $hdc, 2)
$g.ReleaseHdc($hdc)
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
"saved $Out ($w x $ht, ok=$ok)"
