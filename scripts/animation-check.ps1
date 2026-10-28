<#
.SYNOPSIS
    Runs GlazeWM with animations on a Windows desktop and records the screen while windows open,
    close and reflow. Used by .github/workflows/animation-check.yaml.

.DESCRIPTION
    Starts the built glazewm.exe with a config that only turns animations on (slowed down, so a
    recording at ~30 fps catches several frames of each), opens and closes Notepad windows, and
    records the primary screen to out\frames as JPEGs named by milliseconds since the start.
    out\events.txt says when each step happened, out\state-*.json is GlazeWM's window list after
    it, and out\checks.txt is what's on screen once everything settles: each Notepad's frame and
    opacity, and any animation overlay left behind. The script exits 1 if a check fails.
#>
param(
    [string]$Bin = 'target\release',
    [string]$Out = 'out',
    [int]$DurationMs = 600
)
$ErrorActionPreference = 'Stop'

New-Item -ItemType Directory -Force "$Out\frames" | Out-Null
$Out = (Resolve-Path $Out).Path
$glazewm = (Resolve-Path "$Bin\glazewm.exe").Path
$cli = (Resolve-Path "$Bin\glazewm-cli.exe").Path

# Animations only, everything else default; slowed so each one spans many frames.
$config = Join-Path $Out 'config.yaml'
@"
general:
  hide_method: 'cloak'
gaps:
  inner_gap: '10px'
  outer_gap: '10px'
animations:
  window_move:
    effect_type: 'slide'
    duration_ms: $DurationMs
    easing: 'ease_out_quint'
    trigger_threshold: '10px'
  window_open:
    effect_type: 'fade'
    duration_ms: $DurationMs
    easing: 'ease_out_cubic'
"@ | Set-Content $config -Encoding utf8

$start = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
function Write-Event($text) {
    $ms = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() - $start
    "$ms`t$text" | Add-Content "$Out\events.txt"
    Write-Host "[$ms ms] $text"
}

# The recorder runs in its own process so captures keep their pace while this one waits.
$recorder = Join-Path $Out 'record.ps1'
@'
param([string]$Dir, [long]$Start, [int]$Seconds)
Add-Type -AssemblyName System.Drawing, System.Windows.Forms
$bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$codec = [System.Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object MimeType -eq 'image/jpeg'
$params = New-Object System.Drawing.Imaging.EncoderParameters 1
$params.Param[0] = New-Object System.Drawing.Imaging.EncoderParameter ([System.Drawing.Imaging.Encoder]::Quality), 70L
$bitmap = New-Object System.Drawing.Bitmap $bounds.Width, $bounds.Height
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$end = [DateTimeOffset]::UtcNow.AddSeconds($Seconds)
while ([DateTimeOffset]::UtcNow -lt $end) {
    $ms = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() - $Start
    $graphics.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
    $bitmap.Save((Join-Path $Dir ('{0:D6}.jpg' -f $ms)), $codec, $params)
    Start-Sleep -Milliseconds 25
}
'@ | Set-Content $recorder -Encoding utf8

Write-Event 'start glazewm'
$wm = Start-Process $glazewm -ArgumentList 'start', '--config', "`"$config`"", '--verbose' -PassThru `
    -RedirectStandardOutput "$Out\glazewm.log" -RedirectStandardError "$Out\glazewm-err.log"
Start-Sleep -Seconds 4
if ($wm.HasExited) { throw "glazewm exited with code $($wm.ExitCode)" }

$rec = Start-Process pwsh -ArgumentList '-NoProfile', '-File', "`"$recorder`"", "`"$Out\frames`"", $start, 30 -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 1

function Save-State($name) {
    & $cli query windows | Set-Content "$Out\state-$name.json"
}

$pause = [Math]::Max(2, [Math]::Ceiling(($DurationMs + 1200) / 1000))
$notepads = @()
foreach ($i in 1..3) {
    Write-Event "open notepad $i"
    $notepads += Start-Process notepad -PassThru
    Start-Sleep -Seconds $pause
    Save-State "open-$i"
}

Write-Event 'close notepad 2'
Stop-Process -Id $notepads[1].Id
Start-Sleep -Seconds $pause
Save-State 'close-2'

Write-Event 'toggle tiling direction'
& $cli command toggle-tiling-direction | Out-Null
Start-Sleep -Seconds $pause
Save-State 'toggle'

Write-Event 'done'
$rec | Wait-Process -Timeout 40 -ErrorAction SilentlyContinue

# What's on screen once everything has settled.
Add-Type @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class Win {
  public delegate bool EnumProc(IntPtr hwnd, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc proc, IntPtr lParam);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr hwnd, StringBuilder name, int max);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
  [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr hwnd, int index);
  [DllImport("user32.dll")] public static extern bool GetLayeredWindowAttributes(IntPtr hwnd, out uint key, out byte alpha, out uint flags);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
  public static List<IntPtr> All() {
    var list = new List<IntPtr>();
    EnumWindows((h, l) => { list.Add(h); return true; }, IntPtr.Zero);
    return list;
  }
  public static string Class(IntPtr hwnd) { var sb = new StringBuilder(256); GetClassName(hwnd, sb, 256); return sb.ToString(); }
}
'@
$failed = $false
$lines = @()
$overlays = [Win]::All() | Where-Object { [Win]::Class($_) -eq 'AnimationWindow' }
$lines += "animation overlays left: $(@($overlays).Count)"
if (@($overlays).Count) { $failed = $true }
$alive = $notepads | Where-Object { -not $_.HasExited } | ForEach-Object { $_.Id }
foreach ($hwnd in [Win]::All()) {
    $procId = 0
    [void][Win]::GetWindowThreadProcessId($hwnd, [ref]$procId)
    if ($alive -notcontains $procId -or -not [Win]::IsWindowVisible($hwnd) -or [Win]::Class($hwnd) -ne 'Notepad') { continue }
    $rect = New-Object Win+RECT
    [void][Win]::GetWindowRect($hwnd, [ref]$rect)
    $layered = ([Win]::GetWindowLong($hwnd, -20) -band 0x80000) -ne 0
    $alpha = 255
    if ($layered) { $key = 0; $a = [byte]0; $flags = 0; if ([Win]::GetLayeredWindowAttributes($hwnd, [ref]$key, [ref]$a, [ref]$flags)) { $alpha = $a } }
    if ($alpha -ne 255) { $failed = $true }
    $lines += "notepad pid $procId at ($($rect.Left),$($rect.Top))-($($rect.Right),$($rect.Bottom)) layered=$layered alpha=$alpha"
}
$lines | Set-Content "$Out\checks.txt"
$lines | Write-Host

$notepads | Where-Object { -not $_.HasExited } | Stop-Process -ErrorAction SilentlyContinue
& $cli command wm-exit 2>$null | Out-Null
Start-Sleep -Seconds 2
if (-not $wm.HasExited) { $wm | Stop-Process }
Copy-Item "$HOME\.glzr\glazewm\errors.log" $Out -ErrorAction SilentlyContinue
if ($failed) { Write-Host 'A check failed: see checks.txt'; exit 1 }
