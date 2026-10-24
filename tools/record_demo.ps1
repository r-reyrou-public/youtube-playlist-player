# デモ動画を録画する（メイン画面を録画。Enter キーで停止）
#   使い方: powershell -ExecutionPolicy Bypass -File tools\record_demo.ps1 [-Out <mp4のパス>] [-Seconds <自動停止秒数>]
#   出力   : 既定はプロジェクト直下の demo\demo_raw.mp4（既存ファイルは上書きしない）
param(
    [string]$Out = (Join-Path (Split-Path $PSScriptRoot) "demo\demo_raw.mp4"),
    [int]$Seconds = 0,        # 0 なら Enter で停止
    [int]$Countdown = 5
)

# ffmpeg の場所（PATH 未反映のシェルでも見つかるように winget の配置先も探す）
$ff = (Get-Command ffmpeg -ErrorAction SilentlyContinue).Source
if (-not $ff) {
    $ff = Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Recurse -Filter ffmpeg.exe -ErrorAction SilentlyContinue |
        Select-Object -First 1 -ExpandProperty FullName
}
if (-not $ff) { Write-Error "ffmpeg が見つかりません"; exit 1 }

if (Test-Path $Out) { Write-Error "既に存在します: $Out （別の -Out を指定するか、先に移動してください）"; exit 1 }
New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null

# メイン モニターの実ピクセル サイズ（DPI スケーリングの影響を受けないようにする）
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System.Runtime.InteropServices;
public class Dpi { [DllImport("user32.dll")] public static extern bool SetProcessDPIAware(); }
"@
[void][Dpi]::SetProcessDPIAware()
$b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$w = $b.Width - ($b.Width % 2); $h = $b.Height - ($b.Height % 2)   # H.264 は偶数サイズ

for ($i = $Countdown; $i -gt 0; $i--) { Write-Host "録画開始まで $i ..."; Start-Sleep 1 }

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $ff
$psi.Arguments = "-hide_banner -loglevel error -f gdigrab -framerate 30 -offset_x $($b.X) -offset_y $($b.Y) -video_size ${w}x${h} -draw_mouse 1 -i desktop " +
                 "-c:v libx264 -preset veryfast -crf 23 -pix_fmt yuv420p -movflags +faststart `"$Out`""
$psi.UseShellExecute = $false
$psi.RedirectStandardInput = $true
$p = [System.Diagnostics.Process]::Start($psi)
Write-Host "● 録画中 (${w}x${h})。$(if ($Seconds -gt 0) { "$Seconds 秒後に自動停止" } else { 'Enter キーで停止' })"

if ($Seconds -gt 0) { Start-Sleep $Seconds } else { [void](Read-Host) }

$p.StandardInput.WriteLine("q")          # ffmpeg に正常終了を伝える（mp4 を壊さない）
if (-not $p.WaitForExit(30000)) { $p.Kill() }
Write-Host "保存しました: $Out"
