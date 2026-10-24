# リリースビルドして、実行ファイルをプロジェクト直下へコピーする
Push-Location "$PSScriptRoot\src-tauri"
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    Copy-Item "target\release\youtube-playlist-player.exe" "$PSScriptRoot\" -Force
    Write-Host "OK: $PSScriptRoot\youtube-playlist-player.exe"
} finally { Pop-Location }
