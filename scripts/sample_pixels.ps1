$cs = @'
using System;
using System.Runtime.InteropServices;
public class Px {
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
$x = 2940
Write-Output "x=$x"
foreach ($y in 352..374) {
  Write-Output ("y={0}  RGB={1}" -f $y, [Px]::Sample($x, $y))
}
