$cs = @'
using System;
using System.Runtime.InteropServices;
public class Px4 {
  [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
  [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr dc);
  [DllImport("gdi32.dll")] public static extern uint GetPixel(IntPtr dc, int x, int y);
  public static string Sample(int x, int y) {
    IntPtr dc = GetDC(IntPtr.Zero);
    uint p = GetPixel(dc, x, y);
    ReleaseDC(IntPtr.Zero, dc);
    int r = (int)(p & 0xFF); int g = (int)((p >> 8) & 0xFF); int b = (int)((p >> 16) & 0xFF);
    return r + "," + g + "," + b;
  }
}
'@
Add-Type -TypeDefinition $cs
Write-Output "=== top screen (2160x1440 at 0,-1440): vertical scan x=1080 ==="
foreach ($y in @(-1440, -1200, -800, -400, -200, -100, -50, -30, -20, -15, -10, -5, -3, -2, -1)) {
  Write-Output ("y={0}  RGB={1}" -f $y, [Px4]::Sample(1080, $y))
}
Write-Output "=== right screen (1920x1080 at 2560,360): horizontal scan y=900 ==="
foreach ($x in @(2560, 2562, 2564, 2566, 2568, 2570, 2575, 2580, 2600, 2800, 3500, 4479)) {
  Write-Output ("x={0}  RGB={1}" -f $x, [Px4]::Sample($x, 900))
}
