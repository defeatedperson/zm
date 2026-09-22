# 诊断脚本：枚举壁纸窗口的样式位、窗口矩形、客户区矩形（只读）
$cs = @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public class WinDump2 {
  delegate bool EnumProc(IntPtr h, IntPtr lp);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr lp);
  [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr parent, EnumProc cb, IntPtr lp);
  [DllImport("user32.dll")] static extern int GetClassName(IntPtr h, StringBuilder sb, int max);
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] static extern int MapWindowPoints(IntPtr from, IntPtr to, ref POINT p, int n);
  [DllImport("user32.dll")] static extern IntPtr GetWindowLongPtr(IntPtr h, int idx);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [StructLayout(LayoutKind.Sequential)] struct RECT { public int L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] struct POINT { public int X, Y; }

  static string Cls(IntPtr h) { var sb = new StringBuilder(256); GetClassName(h, sb, 256); return sb.ToString(); }
  static string WRect(IntPtr h) { RECT r; GetWindowRect(h, out r); return "(" + r.L + "," + r.T + ")-(" + r.R + "," + r.B + ") [" + (r.R - r.L) + "x" + (r.B - r.T) + "]"; }
  static string CRect(IntPtr h) {
    RECT r; GetClientRect(h, out r);
    POINT p = new POINT(); MapWindowPoints(h, IntPtr.Zero, ref p, 1);
    return "client=[" + (r.R - r.L) + "x" + (r.B - r.T) + "] origin=(" + p.X + "," + p.Y + ")";
  }
  static string Style(IntPtr h) { return "style=0x" + GetWindowLongPtr(h, -16).ToInt64().ToString("X") + " ex=0x" + GetWindowLongPtr(h, -20).ToInt64().ToString("X"); }

  static List<IntPtr> Children(IntPtr p) {
    var list = new List<IntPtr>();
    EnumChildWindows(p, delegate(IntPtr h, IntPtr l) { list.Add(h); return true; }, IntPtr.Zero);
    return list;
  }

  public static List<string> Dump() {
    var lines = new List<string>();
    IntPtr workerw = IntPtr.Zero;
    IntPtr progman = IntPtr.Zero;
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      if (Cls(h) == "Progman") { progman = h; return false; }
      return true;
    }, IntPtr.Zero);
    if (progman != IntPtr.Zero) {
      foreach (var w in Children(progman)) {
        if (Cls(w) != "WorkerW" || !IsWindowVisible(w)) continue;
        foreach (var x in Children(w)) {
          if (Cls(x) != "Tauri Window") continue;
          lines.Add("[wallpaper] TauriWindow hwnd=" + x + " " + WRect(x) + " " + CRect(x) + " " + Style(x));
          foreach (var c in Children(x)) {
            lines.Add("  child " + Cls(c) + " hwnd=" + c + " " + WRect(c));
          }
        }
      }
    }
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      var cls = Cls(h);
      if (cls == "Tauri Window") {
        lines.Add("[main] TauriWindow hwnd=" + h + " " + WRect(h) + " " + CRect(h) + " " + Style(h));
        foreach (var c in Children(h)) {
          var ccls = Cls(c);
          lines.Add("  child " + ccls + " hwnd=" + c + " " + WRect(c));
        }
      }
      return true;
    }, IntPtr.Zero);
    return lines;
  }
}
'@
Add-Type -TypeDefinition $cs
[WinDump2]::Dump() | ForEach-Object { Write-Output $_ }
