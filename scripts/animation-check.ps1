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

    checks.txt also has a timing table, from GlazeWM's own log (tracing target glazewm::anim):
    how long each animation took to start and to capture its window, and how evenly its frames
    came. Timings are reported, never failed on: the runner has no GPU, so they vary run to run.
    What fails a run is always the same on any machine: an overlay left behind, a window left
    transparent, or a window that isn't exactly where GlazeWM's layout puts it.

    One Notepad is closed with GlazeWM's close command, the close animation's only trigger: its
    snapshot must be up right after the command and gone once the animation has had its time.
    With -Disabled the config says animations.enabled: false, and no overlay may appear at all.
#>
param(
    [string]$Bin = 'target\release',
    [string]$Out = 'out',
    [int]$DurationMs = 600,
    # The fade-in of new windows; the same as the slides when left out.
    [int]$OpenMs = 0,
    # The fade-out of closed windows; 80% of the slides when left out (exits are quicker).
    [int]$CloseMs = 0,
    # animations.enabled: false, with every other setting kept.
    [switch]$Disabled
)
$ErrorActionPreference = 'Stop'
if ($OpenMs -le 0) { $OpenMs = $DurationMs }
if ($CloseMs -le 0) { $CloseMs = [Math]::Max(1, [int]($DurationMs * 0.8)) }

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
  outer_gap: { top: '10px', right: '10px', bottom: '10px', left: '10px' }
workspaces:
  - name: '1'
  - name: '2'
animations:
  enabled: $(if ($Disabled) { 'false' } else { 'true' })
  window_close:
    effect_type: 'popin'
    duration_ms: $CloseMs
    easing: 'ease_out_quint'
  window_move:
    effect_type: 'slide'
    duration_ms: $DurationMs
    easing: 'ease_out_quint'
    trigger_threshold: '10px'
  window_open:
    effect_type: 'fade'
    duration_ms: $OpenMs
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

# GlazeWM's own output, in the job's log: what went wrong is there when a step fails.
function Show-Logs {
    foreach ($log in "$Out\glazewm.log", "$Out\glazewm-err.log", "$HOME\.glzr\glazewm\errors.log") {
        if (Test-Path $log) { Write-Host "--- $log (last 60 lines)"; Get-Content $log -Tail 60 | Write-Host }
    }
}

# Windows' privacy switch for capturing windows without the yellow border (Settings > Privacy &
# security), for desktop apps. Without it, GlazeWM's snapshots briefly outline each window.
$consent = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\graphicsCaptureWithoutBorder'
foreach ($key in $consent, "$consent\NonPackaged") {
    New-Item -Path $key -Force | Out-Null
    Set-ItemProperty -Path $key -Name Value -Value 'Allow'
}

Write-Event 'start glazewm'
Write-Host "session $([Diagnostics.Process]::GetCurrentProcess().SessionId), interactive $([Environment]::UserInteractive), explorer $([bool](Get-Process explorer -ErrorAction SilentlyContinue))"
$wm = Start-Process $glazewm -ArgumentList 'start', '--config', "`"$config`"", '--verbose' -PassThru `
    -RedirectStandardOutput "$Out\glazewm.log" -RedirectStandardError "$Out\glazewm-err.log"
# Up once its IPC server answers (a config error leaves it waiting on a dialog instead).
$ready = $false
foreach ($i in 1..30) {
    Start-Sleep -Milliseconds 500
    if ($wm.HasExited) { break }
    & $cli query monitors *> $null
    if ($LASTEXITCODE -eq 0) { $ready = $true; break }
}
if (-not $ready) {
    Show-Logs
    if (-not $wm.HasExited) { $wm | Stop-Process }
    throw 'GlazeWM did not start (see its logs above).'
}
Write-Event 'glazewm ready'

# With $env:FFMPEG set (the workflow does), the screen is recorded as video at up to 60 fps, with
# each frame's real time kept, to out\screen.mp4, and the frames for the contact sheets are cut
# from it afterwards. Otherwise screenshots are taken in a loop. Only one of the two runs: on a
# machine without a GPU, both together halve the frames the compositor manages.
$ffmpeg = $null
if ($env:FFMPEG -and (Test-Path $env:FFMPEG)) {
    $ffmpegInfo = New-Object System.Diagnostics.ProcessStartInfo $env:FFMPEG
    # Info level for the input's start time (wall clock), which places the frames in time.
    $ffmpegInfo.Arguments = "-y -loglevel info -nostats -f gdigrab -framerate 60 -draw_mouse 0 -i desktop " +
        "-fps_mode passthrough -c:v libx264 -preset ultrafast -crf 20 -pix_fmt yuv420p `"$Out\screen.mp4`""
    $ffmpegInfo.UseShellExecute = $false
    $ffmpegInfo.RedirectStandardInput = $true
    $ffmpegInfo.RedirectStandardError = $true
    $ffmpeg = [System.Diagnostics.Process]::Start($ffmpegInfo)
    $ffmpegErrors = $ffmpeg.StandardError.ReadToEndAsync()
}

$rec = $null
if (-not $ffmpeg) {
    $rec = Start-Process pwsh -ArgumentList '-NoProfile', '-File', "`"$recorder`"", "`"$Out\frames`"", $start, 30 -PassThru -WindowStyle Hidden
}
Start-Sleep -Seconds 1

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
  // DWMWA_EXTENDED_FRAME_BOUNDS (9): the frame as seen, without the invisible resize borders.
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr hwnd, int attr, out RECT rect, int size);
  public static RECT Visible(IntPtr hwnd) {
    RECT rect;
    if (DwmGetWindowAttribute(hwnd, 9, out rect, Marshal.SizeOf(typeof(RECT))) != 0) GetWindowRect(hwnd, out rect);
    return rect;
  }
  public static List<IntPtr> All() {
    var list = new List<IntPtr>();
    EnumWindows((h, l) => { list.Add(h); return true; }, IntPtr.Zero);
    return list;
  }
  public static string Class(IntPtr hwnd) { var sb = new StringBuilder(256); GetClassName(hwnd, sb, 256); return sb.ToString(); }
}
'@

# GlazeWM's animation overlays (their window class, platform_impl/windows/animation_window.rs).
function Get-Overlays { @([Win]::All() | Where-Object { [Win]::Class($_) -eq 'AnimationWindow' }) }
# Any overlay seen after a step, for -Disabled.
$overlaysSeen = 0

function Save-State($name) {
    & $cli query windows | Set-Content "$Out\state-$name.json"
    $script:overlaysSeen += (Get-Overlays).Count
}

$pause = [Math]::Max(2, [Math]::Ceiling(($DurationMs + 1200) / 1000))
$notepads = @()
foreach ($i in 1..3) {
    Write-Event "open notepad $i"
    $notepads += Start-Process notepad -PassThru
    Start-Sleep -Seconds $pause
    Save-State "open-$i"
}

# Closed through GlazeWM, the close animation's trigger: the command returns once the snapshot
# is up, so it must be there now (and, with animations off, must not).
Write-Event 'close notepad 2'
$notepads[1].Refresh()
$closing = (& $cli query windows | ConvertFrom-Json).data.windows |
    Where-Object { $_.handle -eq [long]$notepads[1].MainWindowHandle } | Select-Object -First 1
if (-not $closing) { throw "GlazeWM doesn't manage notepad 2 (handle $($notepads[1].MainWindowHandle))." }
& $cli command --id $closing.id close | Out-Null
$upNow = (Get-Overlays).Count
$closeOverlayUp = $upNow -gt 0
$overlaysSeen += $upNow
Write-Event "close overlays up: $upNow"
Start-Sleep -Seconds $pause
Save-State 'close-2'

# The focused Notepad swaps places with its neighbor: two windows slide.
Write-Event 'move window left'
& $cli command focus --container-id ((& $cli query windows | ConvertFrom-Json).data.windows |
        Where-Object processName -eq 'notepad' | Select-Object -Last 1).id | Out-Null
& $cli command move --direction left | Out-Null
Start-Sleep -Seconds $pause
Save-State 'move'

Write-Event 'done'
if ($rec) { $rec | Wait-Process -Timeout 40 -ErrorAction SilentlyContinue }
if ($ffmpeg) {
    Start-Sleep -Seconds 1
    # 'q' asks ffmpeg to finish the file.
    $ffmpeg.StandardInput.Write('q')
    $ffmpeg.StandardInput.Close()
    if (-not $ffmpeg.WaitForExit(20000)) { $ffmpeg.Kill() }
    $ffmpegErrors.Result | Set-Content "$Out\ffmpeg.log"

    # The frames, as JPEGs named by milliseconds since the start like the screenshots: the
    # input's start (unix seconds) places frame 0, and showinfo gives each frame's time after it.
    $videoStart = Select-String -Path "$Out\ffmpeg.log" -Pattern 'start: ([0-9.]+)' | Select-Object -First 1
    if ($videoStart -and (Test-Path "$Out\screen.mp4")) {
        $offsetMs = [double]$videoStart.Matches[0].Groups[1].Value * 1000 - $start
        $cut = "$Out\cut"
        New-Item -ItemType Directory -Force $cut | Out-Null
        $info = & $env:FFMPEG -hide_banner -i "$Out\screen.mp4" -vf showinfo -fps_mode passthrough -q:v 4 "$cut\%06d.jpg" 2>&1 |
            Select-String -Pattern 'n:\s*(\d+).*pts_time:([0-9.]+)'
        foreach ($frame in $info) {
            $index = [int]$frame.Matches[0].Groups[1].Value + 1
            $ms = [int]($offsetMs + [double]$frame.Matches[0].Groups[2].Value * 1000)
            $file = '{0}\{1:D6}.jpg' -f $cut, $index
            if ($ms -ge 0 -and (Test-Path $file)) { Move-Item $file ('{0}\frames\{1:D6}.jpg' -f $Out, $ms) -Force }
        }
        Remove-Item $cut -Recurse -Force
    }
}

# What's on screen once everything has settled.
$failed = $false
$lines = @()
if ($Disabled) {
    $lines += "animations disabled; overlays seen during the steps: $overlaysSeen"
    if ($overlaysSeen) { $failed = $true }
} else {
    $lines += "close overlay up right after the close command: $closeOverlayUp"
    if (-not $closeOverlayUp) { $failed = $true }
}
$overlays = Get-Overlays
$lines += "animation overlays left: $(@($overlays).Count)"
if (@($overlays).Count) { $failed = $true }
$alive = $notepads | Where-Object { -not $_.HasExited } | ForEach-Object { $_.Id }
# Where GlazeWM's layout puts each window, by handle.
$layout = @{}
foreach ($w in (& $cli query windows | ConvertFrom-Json).data.windows) { $layout[[long]$w.handle] = $w }
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
    # The visible frame must be the layout's rect once animations are over.
    $want = $layout[[long]$hwnd]
    $seen = [Win]::Visible($hwnd)
    if (-not $want) {
        $lines += "  not in GlazeWM's layout"
        $failed = $true
    } else {
        $exact = $seen.Left -eq $want.x -and $seen.Top -eq $want.y -and
            ($seen.Right - $seen.Left) -eq $want.width -and ($seen.Bottom - $seen.Top) -eq $want.height
        $lines += "  visible ($($seen.Left),$($seen.Top)) $($seen.Right - $seen.Left)x$($seen.Bottom - $seen.Top), layout ($($want.x),$($want.y)) $($want.width)x$($want.height): $(if ($exact) { 'exact' } else { 'OFF' })"
        if (-not $exact) { $failed = $true }
    }
}

# Timing, from GlazeWM's log: reported, not checked (see the description at the top).
function Get-Field($line, $name) { if ($line -match "\b$name=([0-9.]+|None|Some\(([0-9]+)\))") { if ($matches[2]) { $matches[2] } else { $matches[1] } } }
function Get-Spread($values) {
    $sorted = @($values | ForEach-Object { [double]$_ } | Sort-Object)
    if (-not $sorted.Count) { return 'none' }
    $median = $sorted[[int][Math]::Floor(($sorted.Count - 1) / 2)]
    "min $($sorted[0]), median $median, max $($sorted[-1]) (n=$($sorted.Count))"
}
$anim = @()
if (Test-Path "$Out\glazewm.log") {
    # Without the log's colors, so fields read as name=value.
    $anim = @(Get-Content "$Out\glazewm.log" | ForEach-Object { $_ -replace "`e\[[0-9;]*m", '' } | Where-Object { $_ -match 'glazewm::anim' })
}
$lines += ''
$lines += 'timing (ms; reported, not checked)'
$lines += "  start (asked for to first frame): $(Get-Spread ($anim | Where-Object { $_ -match 'First animation frame' } | ForEach-Object { Get-Field $_ 'start_ms' }))"
$lines += "  window capture: $(Get-Spread ($anim | Where-Object { $_ -match 'Window captured' } | ForEach-Object { Get-Field $_ 'capture_ms' }))"
$lines += '  compositor frames during each run (what is seen): frames | interval mean / p95 / max | late (>1.5 periods) | period'
foreach ($run in $anim | Where-Object { $_ -match 'Compositor frames' }) {
    $f = { param($n) Get-Field $run $n }
    $lines += "    $(& $f frames) | $(& $f mean_ms) / $(& $f p95_ms) / $(& $f max_ms) | $(& $f late) | $(& $f period_ms)"
}
$lines += '  WM ticks during each run (0 frames: the compositor played it alone): frames | interval mean / p95 / max | late | period | duration | dwm composed / expected, dropped'
foreach ($run in $anim | Where-Object { $_ -match 'Animation frames' }) {
    $f = { param($n) Get-Field $run $n }
    $lines += "    $(& $f frames) | $(& $f mean_ms) / $(& $f p95_ms) / $(& $f max_ms) | $(& $f late) | $(& $f period_ms) | $(& $f duration_ms) | $(& $f dwm_composed) / $(& $f dwm_expected), $(& $f dwm_dropped)"
}
$lines | Set-Content "$Out\checks.txt"
$lines | Write-Host

$notepads | Where-Object { -not $_.HasExited } | Stop-Process -ErrorAction SilentlyContinue
& $cli command wm-exit *> $null
Start-Sleep -Seconds 2
if (-not $wm.HasExited) { $wm | Stop-Process }
Copy-Item "$HOME\.glzr\glazewm\errors.log" $Out -ErrorAction SilentlyContinue
Show-Logs
if ($failed) { Write-Host 'A check failed: see checks.txt'; exit 1 }
exit 0
