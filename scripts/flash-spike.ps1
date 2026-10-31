<#
.SYNOPSIS
    Experiment on the new-window flash (docs/hyprland-parity.md): which ways of hiding a window keep
    it, title bar included, off screen from the moment it's shown? Used by
    .github/workflows/flash-spike.yaml.

.DESCRIPTION
    Records the screen at 60 fps (ffmpeg, from $env:FFMPEG) while a test window (magenta, with a
    title bar) is shown once per variant, at the same place:

      plain           shown as is (what a flash looks like)
      alpha           made layered at alpha 0 by itself, before it's shown
      alpha-notrans   the same, plus DWM's open transition turned off (DWMWA_TRANSITIONS_FORCEDISABLED)
      cloak           cloaked by itself (DWMWA_CLOAK) before it's shown
      offscreen       shown at -32000,-32000, then moved into place
      ext-alpha       hidden by another process when created (alpha 0), as GlazeWM does
      ext-alpha-notrans  the same, plus the open transition turned off by that process
      ext-cloak       the other process tries DWMWA_CLOAK (allowed only on its own windows?)

    Hidden variants are revealed 600 ms after being shown. out-flash\events.txt has each step's
    unix time in ms; out-flash\ffmpeg.log has the recording's start; out-flash\hook.txt says what
    the other process did. The frames are compared afterwards (anything drawn in the window's place
    between show and reveal is a flash).
#>
param([string]$Out = 'out-flash')
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path

$native = @'
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
'@

# The other process: hooks window creation and hides test windows titled "ext-*".
$hook = Join-Path $Out 'hook.ps1'
@"
Add-Type -AssemblyName System.Windows.Forms
Add-Type @'
$native
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
      if (title == "ext-alpha-notrans") Log.Add(title + " no transitions: " + N.NoTransitions(h));
      Log.Add(title + " hidden at create");
    };
    SetWinEventHook(0x8000, 0x8000, IntPtr.Zero, keep, 0, 0, 0);
  }
}
'@
[H]::Start()
`$end = [DateTime]::UtcNow.AddSeconds(40)
while ([DateTime]::UtcNow -lt `$end) { [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 1
  if (`$([H]::Log.Count)) { [H]::Log | Add-Content "$Out\hook.txt"; [H]::Log.Clear() } }
"@ | Set-Content $hook -Encoding utf8
$hookProcess = Start-Process pwsh -ArgumentList '-NoProfile', '-File', "`"$hook`"" -PassThru -WindowStyle Hidden

Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type $native

function Write-Event($text) {
    "$([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())`t$text" | Add-Content "$Out\events.txt"
}

# The recording.
$ffInfo = New-Object System.Diagnostics.ProcessStartInfo $env:FFMPEG
$ffInfo.Arguments = "-y -loglevel info -nostats -f gdigrab -framerate 60 -draw_mouse 0 -i desktop -fps_mode passthrough -c:v libx264 -preset ultrafast -crf 18 -pix_fmt yuv420p `"$Out\screen.mp4`""
$ffInfo.UseShellExecute = $false; $ffInfo.RedirectStandardInput = $true; $ffInfo.RedirectStandardError = $true
$ff = [System.Diagnostics.Process]::Start($ffInfo)
$ffErr = $ff.StandardError.ReadToEndAsync()
Start-Sleep -Seconds 3

function Wait-Ms($ms) {
    $until = [DateTime]::UtcNow.AddMilliseconds($ms)
    while ([DateTime]::UtcNow -lt $until) { [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 2 }
}

foreach ($variant in 'plain', 'alpha', 'alpha-notrans', 'cloak', 'offscreen', 'ext-alpha', 'ext-alpha-notrans', 'ext-cloak') {
    $form = New-Object System.Windows.Forms.Form
    $form.Text = $variant
    $form.BackColor = [System.Drawing.Color]::Magenta
    $form.StartPosition = 'Manual'
    $form.Bounds = New-Object System.Drawing.Rectangle 300, 150, 500, 400
    if ($variant -eq 'offscreen') { $form.Location = New-Object System.Drawing.Point -32000, -32000 }
    $h = $form.Handle   # Creates the window, not yet shown.
    switch ($variant) {
        'alpha' { [N]::Alpha($h, 0) }
        'alpha-notrans' { [N]::Alpha($h, 0); $r = [N]::NoTransitions($h); Write-Event "$variant no transitions: $r" }
        'cloak' { $r = [N]::Cloak($h, $true); Write-Event "$variant cloak: $r" }
    }
    Wait-Ms 150
    Write-Event "show $variant"
    $form.Show()
    Wait-Ms 600
    Write-Event "reveal $variant"
    switch -Wildcard ($variant) {
        'cloak' { [void][N]::Cloak($h, $false) }
        'offscreen' { [void][N]::SetWindowPos($h, [IntPtr]::Zero, 300, 150, 0, 0, 0x1 -bor 0x4 -bor 0x10) }
        'plain' { }
        'ext-cloak' { }
        default { [N]::Alpha($h, 255) }
    }
    Wait-Ms 500
    Write-Event "close $variant"
    $form.Close(); $form.Dispose()
    Wait-Ms 700
}

$ff.StandardInput.Write('q'); $ff.StandardInput.Close()
if (-not $ff.WaitForExit(20000)) { $ff.Kill() }
$ffErr.Result | Set-Content "$Out\ffmpeg.log"
$hookProcess | Stop-Process -ErrorAction SilentlyContinue
