Add-Type -AssemblyName System.Windows.Forms
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class N {
  [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h, int i);
  [DllImport("user32.dll")] public static extern int SetWindowLong(IntPtr h, int i, int v);
  [DllImport("user32.dll")] public static extern bool SetLayeredWindowAttributes(IntPtr h, uint key, byte alpha, uint flags);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint f);
  [DllImport("dwmapi.dll")] public static extern int DwmSetWindowAttribute(IntPtr h, int attr, ref int value, int size);
  public static void Alpha(IntPtr h, byte a) {
    SetWindowLong(h, -20, GetWindowLong(h, -20) | 0x80000);
    SetLayeredWindowAttributes(h, 0, a, 2);
  }
  public static int NoTransitions(IntPtr h) { int v = 1; return DwmSetWindowAttribute(h, 3, ref v, 4); }
  public static int Cloak(IntPtr h, bool on) { int v = on ? 1 : 0; return DwmSetWindowAttribute(h, 13, ref v, 4); }
}
public static class H {
  public delegate void Proc(IntPtr hook, uint ev, IntPtr h, int obj, int child, uint thread, uint time);
  [DllImport("user32.dll")] public static extern IntPtr SetWinEventHook(uint min, uint max, IntPtr mod, Proc p, uint pid, uint tid, uint flags);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, System.Text.StringBuilder s, int max);
  public static System.Collections.Generic.List<string> Log = new System.Collections.Generic.List<string>();
  static Proc keep;
  public static void Start() {
    keep = (hk, ev, h, obj, child, th, t) => {
      if (obj != 0 || child != 0) return;
      var sb = new System.Text.StringBuilder(64); GetWindowText(h, sb, 64); var title = sb.ToString();
      if (!title.StartsWith("ext-")) return;
      if (title == "ext-cloak") { Log.Add(title + " cloak: " + N.Cloak(h, true)); return; }
      N.Alpha(h, 0);
      Log.Add(title + " hook latency ms: " + (unchecked((uint)Environment.TickCount - t)));
      if (title == "ext-alpha-notrans") Log.Add(title + " no transitions: " + N.NoTransitions(h));
      Log.Add(title + " hidden at create");
    };
    SetWinEventHook(0x8000, 0x8000, IntPtr.Zero, keep, 0, 0, 0);
  }
}
'@
# Highest priority for the hook's thread, so it wakes as soon as an event is posted.
[System.Threading.Thread]::CurrentThread.Priority = 'Highest'
[System.Diagnostics.Process]::GetCurrentProcess().PriorityClass = 'High'
[H]::Start()
$end = [DateTime]::UtcNow.AddSeconds(90)
while ([DateTime]::UtcNow -lt $end) { [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 1
  if ($([H]::Log.Count)) { [H]::Log | Add-Content "D:\a\glazewm\glazewm\out-flash\hook.txt"; [H]::Log.Clear() } }
