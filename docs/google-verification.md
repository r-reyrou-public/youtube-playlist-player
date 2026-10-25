# Google OAuth 審査（アプリの確認）申請ガイド

このアプリが使う `https://www.googleapis.com/auth/youtube` は「機密スコープ」です。
審査を通すと「このアプリは確認されていません」の警告が消え、テストユーザー100人の上限と7日での認可失効がなくなります。
申請は **あなたの Google アカウントで Google Cloud Console から** 行います（代行できません）。

## 申請前に揃えるもの

| # | 必要なもの | 状態 | 備考 |
|---|---|---|---|
| 1 | アプリのホームページ（公開URL） | 未 | GitHub Pages で `docs/index.md` 等を公開するのが手軽。ドメインの所有確認が必要（下記） |
| 2 | プライバシーポリシー（公開URL） | 下書きあり | `docs/privacy-policy.md`。連絡先メールを入れてから公開 |
| 3 | 承認済みドメインの所有確認 | 未 | Search Console で確認。`github.io` のようなサブドメインは所有確認できないため、**独自ドメインが必要になる場合がある** |
| 4 | アプリのロゴ（120x120px） | あり | `src-tauri/icons/icon.png` から作成可 |
| 5 | スコープ利用の説明文 | 下書きあり | 下記「提出文面」 |
| 6 | デモ動画（YouTube に限定公開でアップロード） | 未 | 下記「動画の撮り方」 |
| 7 | サポート用メールアドレス | 要入力 | 同意画面に表示される |

## 手順
1. [Google Auth Platform](https://console.cloud.google.com/auth/overview) →「ブランディング」: アプリ名、ロゴ、ホームページ、プライバシーポリシー、承認済みドメインを入力。
2. 「対象（Audience）」: 公開ステータスを**本番環境**にする。
3. 「データアクセス」: スコープ `…/auth/youtube` を追加し、利用理由を入力（下記）。
4. 「確認センター」から**確認をリクエスト**。デモ動画のURLを添付。
5. Google からのメール（登録アドレス宛）に返信しながら進める。数日〜数週間かかります。

## 提出文面（スコープ利用の説明・英語推奨）

**Why the app needs `https://www.googleapis.com/auth/youtube`:**

> YouTube Playlist Player is a desktop application that lets a user manage their own YouTube playlists: view them, reorder the videos, add videos to other playlists, remove videos from a playlist, and create new playlists. All edits are made locally first, and are sent to YouTube only when the user presses the Push button and confirms. Pushing requires write access: `playlistItems.update` (change the position of an item), `playlistItems.insert` (add a video to a playlist), `playlistItems.delete` (remove a video from a playlist; the video itself is not deleted) and `playlists.insert` (create a new, private playlist). None of this is possible with a read-only scope. The app also reads the user's playlists (`playlists.list`, `playlistItems.list`) to display them. The app never deletes playlists, never uploads or deletes videos, and does not access comments, subscriptions or account settings. It does not send any data to a server operated by the developer. All data stays on the user's device.

**Limited Use statement（ホームページ/ポリシーに明記が必要）:**

> The use of information received from Google APIs will adhere to the Google API Services User Data Policy, including the Limited Use requirements.

## デモ動画の撮り方（英語の字幕か音声があると通りやすい）
1. 同意画面が出るところから開始: アプリで「マイリスト取得」→ブラウザで Google のログイン → **同意画面（アプリ名・要求スコープが見える状態）** を映す。URL バーの **クライアントID（`client_id=`）が見える**ようにする。
2. アプリに戻り、プレイリストが表示されることを見せる。
3. 並べ替え（シャッフル/ドラッグ）→ Push → YouTube 側（ブラウザ）でプレイリストの順序が変わったことを見せる。
4. 「このスコープでできること」を一言で説明する（上の説明文の要約）。

## クォータ（1日10,000ユニット）について
審査とは別に、利用者が増えて上限に達する場合は「YouTube API Services 割り当て拡張」フォームの申請（コンプライアンス監査）が必要です。
書き戻しは 50 ユニット/件なので、全利用者合計で 1 日約 200 件が上限です。

## 注意
- 審査中に OAuth クライアントのスコープや画面名を変えると、やり直しになることがあります。
- 審査が通っても、埋め込み版 exe に含まれるクライアントシークレットは解析で取り出せます（デスクトップアプリでは一般的で、Google もそれを前提にしています）。
