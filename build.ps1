# リリースビルドして、実行ファイルをプロジェクト直下へコピーする
#   通常:    .\build.ps1          ... 利用者が設定画面で自分の OAuth クライアントを登録する版
#   配布用:  .\build.ps1 -Embed   ... 直下の client_secret.json を exe に埋め込む版（利用者はログインだけで使える）
# 注意: -Embed の exe にはクライアントシークレットが含まれる。Git には含めないこと（exe は .gitignore 済み）。
param([switch]$Embed)

Push-Location "$PSScriptRoot\src-tauri"
try {
    Remove-Item Env:YT_CLIENT_ID, Env:YT_CLIENT_SECRET -ErrorAction SilentlyContinue
    if ($Embed) {
        $path = "$PSScriptRoot\client_secret.json"
        if (-not (Test-Path $path)) { Write-Error "client_secret.json が見つかりません: $path"; exit 1 }
        $c = (Get-Content $path -Raw -Encoding UTF8 | ConvertFrom-Json).installed
        if (-not $c) { Write-Error "デスクトップ アプリ形式（installed）の JSON ではありません"; exit 1 }
        $env:YT_CLIENT_ID = $c.client_id
        $env:YT_CLIENT_SECRET = $c.client_secret
        Write-Host "OAuth クライアントを埋め込みます"
        Write-Warning "この exe にはクライアントシークレットが含まれ、解析すれば取り出せます。第三者へ配布する前に、その前提で問題ないか確認してください。"
    }
    cargo build --release
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    Copy-Item "target\release\youtube-playlist-player.exe" "$PSScriptRoot\" -Force
    Write-Host "OK: $PSScriptRoot\youtube-playlist-player.exe"
} finally {
    Remove-Item Env:YT_CLIENT_ID, Env:YT_CLIENT_SECRET -ErrorAction SilentlyContinue
    Pop-Location
}
