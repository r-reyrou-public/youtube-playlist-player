# YouTube Playlist Player (Rust / Tauri v2)

プレイリストを取得 → アプリ内で並べ替え・再生 → 必要なら YouTube へ書き戻すデスクトップアプリ。
バックエンドは Rust、画面は `web/`（HTML/JS）を OS 標準の WebView2 で表示する。

## セットアップ
1. Google Cloud Console で YouTube Data API v3 を有効化し、OAuth クライアント（種類: デスクトップアプリ）を作成。
   JSON を `client_secret.json` として exe と同じフォルダ（開発時はプロジェクト直下）に置く。
2. `cd src-tauri && cargo build --release`
3. `src-tauri/target/release/youtube-playlist-player.exe` を起動（初回のみブラウザで認可。以後 `token.json` で自動更新）

## 補足
- 並べ替え・再生はクォータ消費 0。書き戻しは 50 ユニット/件。
- 書き戻しはサーバー側の並びをシミュレートし、必要な更新だけ実行する。
- 書き戻し対象は「手動並べ替え」設定のプレイリストであること。
- テスト: `cd src-tauri && cargo test`
