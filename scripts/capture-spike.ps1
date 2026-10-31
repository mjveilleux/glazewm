<#
.SYNOPSIS
    Spike for workspace transitions (docs/hyprland-parity.md, phase 3.1): what Windows.Graphics.Capture
    gets from windows that are cloaked (on a hidden workspace) or fully transparent (layered alpha 0).
    Used by .github/workflows/capture-spike.yaml.

.DESCRIPTION
    Starts GlazeWM (animations off, cloaking as the hide method) with Notepad, Edge (Chromium) and
    Windows Terminal (a flip-model swap chain) on workspace 1, then captures each window three ways
    with target\release\examples\capture_spike.exe: as it is, with workspace 2 shown (so workspace
    1's windows are cloaked), and back on workspace 1 at alpha 0. A screenshot of the alpha 0 step
    shows whether each window is really invisible. Results go to out-spike\spike.txt.
#>
param(
    [string]$Bin = 'target\release',
    [string]$Out = 'out-spike'
)
$ErrorActionPreference = 'Stop'

New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$glazewm = (Resolve-Path "$Bin\glazewm.exe").Path
$cli = (Resolve-Path "$Bin\glazewm-cli.exe").Path
$spike = (Resolve-Path "$Bin\examples\capture_spike.exe").Path
$report = "$Out\spike.txt"

$config = Join-Path $Out 'config.yaml'
@"
general:
  hide_method: 'cloak'
workspaces:
  - name: '1'
  - name: '2'
# New windows animate in, so GlazeWM hides them as they're created: the apps below must still
# open and draw normally under it.
animations:
  windows_in: { style: 'popin', duration_ms: 150 }
"@ | Set-Content $config -Encoding utf8

# Borderless capture, as in the animation check.
$consent = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\graphicsCaptureWithoutBorder'
foreach ($key in $consent, "$consent\NonPackaged") {
    New-Item -Path $key -Force | Out-Null
    Set-ItemProperty -Path $key -Name Value -Value 'Allow'
}

function Write-Report($text) { $text | Add-Content $report; Write-Host $text }

function Save-Screen($name) {
    Add-Type -AssemblyName System.Drawing, System.Windows.Forms
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bitmap = New-Object System.Drawing.Bitmap $bounds.Width, $bounds.Height
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
    $bitmap.Save("$Out\$name.png")
    $graphics.Dispose(); $bitmap.Dispose()
}

$wm = Start-Process $glazewm -ArgumentList 'start', '--config', "`"$config`"" -PassThru `
    -RedirectStandardOutput "$Out\glazewm.log" -RedirectStandardError "$Out\glazewm-err.log"
$ready = $false
foreach ($i in 1..30) {
    Start-Sleep -Milliseconds 500
    if ($wm.HasExited) { break }
    & $cli query monitors *> $null
    if ($LASTEXITCODE -eq 0) { $ready = $true; break }
}
if (-not $ready) { throw 'GlazeWM did not start.' }

Start-Process notepad | Out-Null
Start-Process msedge -ArgumentList '--new-window', '--no-first-run', '--inprivate', 'about:blank' | Out-Null
Start-Process wt -ErrorAction SilentlyContinue | Out-Null
Start-Sleep -Seconds 8

# The windows GlazeWM manages on workspace 1, by process.
$windows = @((& $cli query windows | ConvertFrom-Json).data.windows |
        Where-Object { $_.processName -in 'notepad', 'msedge', 'WindowsTerminal' })
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class Alpha {
  [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h, int i);
  [DllImport("user32.dll")] public static extern bool GetLayeredWindowAttributes(IntPtr h, out uint key, out byte alpha, out uint flags);
  public static string Of(long handle) {
    IntPtr h = new IntPtr(handle);
    bool layered = (GetWindowLong(h, -20) & 0x80000) != 0;
    uint key, flags; byte alpha = 255;
    if (layered && !GetLayeredWindowAttributes(h, out key, out alpha, out flags)) return "layered, unreadable";
    return (layered ? "layered" : "not layered") + ", alpha " + alpha;
  }
}
'@
# Opened under GlazeWM, which hid them as they were created: each must have its opacity back.
foreach ($w in $windows) { Write-Report "window $($w.handle): $($w.processName) '$($w.title)': $([Alpha]::Of([long]$w.handle))" }
$handles = @($windows | ForEach-Object { [string]$_.handle })
if (-not $handles.Count) { throw 'No windows to capture.' }

Write-Report ''
Write-Report '1. visible:'
& $spike capture @handles | ForEach-Object { Write-Report "   $_" }
Save-Screen '1-visible'

Write-Report '2. cloaked (workspace 2 shown):'
& $cli command focus --workspace 2 | Out-Null
Start-Sleep -Seconds 1
& $spike capture @handles | ForEach-Object { Write-Report "   $_" }
Save-Screen '2-cloaked'

Write-Report '3. transparent (workspace 1, alpha 0):'
& $cli command focus --workspace 1 | Out-Null
Start-Sleep -Seconds 1
& $spike transparent @handles | ForEach-Object { Write-Report "   $_" }
Start-Sleep -Milliseconds 500
Save-Screen '3-transparent'
& $spike opaque @handles | ForEach-Object { Write-Report "   $_" }

& $cli command wm-exit *> $null
Start-Sleep -Seconds 2
if (-not $wm.HasExited) { $wm | Stop-Process }
