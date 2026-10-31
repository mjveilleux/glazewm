<#
.SYNOPSIS
    Spike for dim_inactive (docs/hyprland-parity.md, phase 5.2): can a helper window dim an inactive
    window by sitting just above it in z-order, and stay right as focus moves? Used by
    .github/workflows/dim-spike.yaml.

.DESCRIPTION
    Opens two Notepads side by side, plus a third one floating over both. A dark, 40% opaque,
    click-through window (the "dim") is placed exactly over Notepad A and ordered just above it.
    Then focus moves around and a screenshot and the z-order are saved after each step:
      1. dim placed over A (B focused)
      2. A is clicked to the front by the OS (as a user focusing it would)
      3. the dim is re-placed above A after the fact (as a WM reacting to focus would)
      4. the floating Notepad C is focused: C must stay above the dim
      5. A opens its Find dialog (an owned popup): it must stay above the dim
    out-dim\dim.txt has the z-order of the four windows after each step; the PNGs show corners and
    stacking.
#>
param([string]$Out = 'out-dim')
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$report = "$Out\dim.txt"

Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class W {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hgt, bool repaint);
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int s);
  public static RECT Frame(IntPtr h) { RECT r; DwmGetWindowAttribute(h, 9, out r, 16); return r; }
  public static List<IntPtr> Order() { var l = new List<IntPtr>(); EnumWindows((h, p) => { if (IsWindowVisible(h)) l.Add(h); return true; }, IntPtr.Zero); return l; }
}
'@

function Write-Report($t) { $t | Add-Content $report; Write-Host $t }
function Save-Screen($name) {
    $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
    $bmp.Save("$Out\$name.png"); $g.Dispose(); $bmp.Dispose()
}

$a = Start-Process notepad -PassThru; $b = Start-Process notepad -PassThru; $c = Start-Process notepad -PassThru
Start-Sleep -Seconds 3
foreach ($p in $a, $b, $c) { $p.Refresh() }
$ha = $a.MainWindowHandle; $hb = $b.MainWindowHandle; $hc = $c.MainWindowHandle
[void][W]::MoveWindow($ha, 20, 20, 480, 600, $true)
[void][W]::MoveWindow($hb, 520, 20, 480, 600, $true)
[void][W]::MoveWindow($hc, 300, 200, 400, 300, $true)

# The dim: borderless, dark, 40% opaque, never activated, click-through.
$dim = New-Object System.Windows.Forms.Form
$dim.FormBorderStyle = 'None'; $dim.ShowInTaskbar = $false; $dim.BackColor = 'Black'; $dim.Opacity = 0.4
$dim.StartPosition = 'Manual'
$dim.Show()
$hd = $dim.Handle
# WS_EX_TRANSPARENT | WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW via GWL_EXSTYLE.
Add-Type -Namespace X -Name U -MemberDefinition '[DllImport("user32.dll")] public static extern int SetWindowLong(IntPtr h, int i, int v); [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h, int i);'
[void][X.U]::SetWindowLong($hd, -20, [X.U]::GetWindowLong($hd, -20) -bor 0x20 -bor 0x80000 -bor 0x08000000 -bor 0x80)

$names = @{ $ha = 'A'; $hb = 'B'; $hc = 'C(floating)'; $hd = 'DIM' }
function Write-Order($step) {
    $order = [W]::Order() | Where-Object { $names.ContainsKey($_) } | ForEach-Object { $names[$_] }
    Write-Report "$step | top to bottom: $($order -join ' > ')"
}

# Exactly over A's visible frame, just above A (insert after the window above A: placing it
# "after A" would put it below A).
function Place-Dim {
    $f = [W]::Frame($ha)
    # SWP_NOACTIVATE 0x10; insert above A by placing A after the dim.
    [void][W]::SetWindowPos($hd, [IntPtr]::Zero, $f.L, $f.T, $f.R - $f.L, $f.B - $f.T, 0x10 -bor 0x04)
    [void][W]::SetWindowPos($ha, $hd, 0, 0, 0, 0, 0x10 -bor 0x01 -bor 0x02)
}

[void][W]::SetForegroundWindow($hb); Start-Sleep -Milliseconds 500
Place-Dim; Start-Sleep -Milliseconds 500
Write-Order '1 dim over A, B focused'; Save-Screen '1-dim-over-a'

[void][W]::SetForegroundWindow($ha); Start-Sleep -Milliseconds 500
Write-Order '2 A focused by the OS'; Save-Screen '2-a-focused'

Place-Dim; Start-Sleep -Milliseconds 500
Write-Order '3 dim re-placed above A'; Save-Screen '3-dim-replaced'

[void][W]::SetForegroundWindow($hc); Start-Sleep -Milliseconds 500
Write-Order '4 C focused'; Save-Screen '4-c-focused'

# Find dialog of A (Ctrl+F through the WM_COMMAND of its menu isn't stable across Notepad
# versions, so the keys are sent).
[void][W]::SetForegroundWindow($ha); Start-Sleep -Milliseconds 300
Place-Dim; Start-Sleep -Milliseconds 300
[System.Windows.Forms.SendKeys]::SendWait('^f'); Start-Sleep -Milliseconds 800
Place-Dim; Start-Sleep -Milliseconds 500
Write-Order '5 A with its Find popup, dim re-placed'; Save-Screen '5-owned-popup'

$dim.Close()
foreach ($p in $a, $b, $c) { $p | Stop-Process -ErrorAction SilentlyContinue }
