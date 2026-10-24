//! YouTube Data API v3 連携（OAuth 2.0 Installed App Flow）。

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use yup_oauth2::authenticator::DefaultAuthenticator;
use yup_oauth2::authenticator_delegate::InstalledFlowDelegate;
use yup_oauth2::{InstalledFlowAuthenticator, InstalledFlowReturnMethod};

use crate::model::{Playlist, PlaylistItem};

const SCOPE: &str = "https://www.googleapis.com/auth/youtube";
const API: &str = "https://www.googleapis.com/youtube/v3/playlistItems";
const PLAYLISTS: &str = "https://www.googleapis.com/youtube/v3/playlists";

/// 接続確立の上限。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 1リクエスト全体（送信〜本文受信）の上限。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// 保存済みトークンの更新（Silent Refresh）の上限。
const REFRESH_TIMEOUT: Duration = Duration::from_secs(30);
/// 初回認可（ブラウザ操作待ち）の上限。
const AUTH_TIMEOUT: Duration = Duration::from_secs(180);

/// 通信エラーを利用者向けのメッセージにする。
fn net_err(e: reqwest::Error) -> String {
    if e.is_timeout() {
        format!("通信がタイムアウトしました（{}秒）。ネットワーク接続を確認して再試行してください。", REQUEST_TIMEOUT.as_secs())
    } else if e.is_connect() {
        "YouTube に接続できませんでした。ネットワーク接続を確認してください。".to_string()
    } else {
        e.to_string()
    }
}

/// URL または ID 文字列から playlistId を取り出す。
pub fn parse_playlist_id(text: &str) -> String {
    if let Some(i) = text.find("list=") {
        let rest = &text[i + 5..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(rest.len());
        return rest[..end].to_string();
    }
    text.trim().to_string()
}

/// client_secret.json を探すディレクトリ（カレント → exe の隣）。token.json も同じ場所に置く。
fn base_dir() -> Result<PathBuf, String> {
    let mut candidates = vec![std::env::current_dir().unwrap_or_default()];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            candidates.push(d.to_path_buf());
            // cargo run / tauri dev 時: target/{debug,release} から遡ってプロジェクト直下も探す
            for up in d.ancestors().skip(1).take(3) {
                candidates.push(up.to_path_buf());
            }
        }
    }
    candidates
        .into_iter()
        .find(|d| d.join("client_secret.json").exists())
        .ok_or_else(|| {
            "client_secret.json が見つかりません。Google Cloud Console でデスクトップアプリ用\
             OAuthクライアントを作成し、exeと同じフォルダ（またはプロジェクト直下）に配置してください。"
                .to_string()
        })
}

/// 認証情報の保存先: 既存の client_secret.json の場所、なければ exe のフォルダ。
pub fn storage_dir() -> PathBuf {
    base_dir().unwrap_or_else(|_| {
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(PathBuf::from))
            .unwrap_or_default()
    })
}

pub fn credentials_configured() -> bool {
    base_dir().is_ok()
}

/// 画面から入力されたクライアントID/シークレットを client_secret.json（デスクトップアプリ形式）として保存。
pub fn save_credentials(client_id: &str, client_secret: &str) -> Result<PathBuf, String> {
    let (id, secret) = (client_id.trim(), client_secret.trim());
    if id.is_empty() || secret.is_empty() {
        return Err("クライアントIDとクライアントシークレットを両方入力してください".into());
    }
    if !id.ends_with(".apps.googleusercontent.com") {
        return Err("クライアントIDの形式が正しくありません（…apps.googleusercontent.com で終わる文字列）".into());
    }
    let dir = storage_dir();
    let json = json!({ "installed": {
        "client_id": id,
        "client_secret": secret,
        "auth_uri": "https://accounts.google.com/o/oauth2/auth",
        "token_uri": "https://oauth2.googleapis.com/token",
        "auth_provider_x509_cert_url": "https://www.googleapis.com/oauth2/v1/certs",
        "redirect_uris": ["http://localhost"],
    }});
    let path = dir.join("client_secret.json");
    std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap()).map_err(|e| e.to_string())?;
    // 別のクライアントで取得した古いトークンは使えないため破棄する
    let _ = std::fs::remove_file(dir.join("token.json"));
    Ok(dir)
}

/// 保存済みトークンを削除（次回アクセス時に再認可）。
pub fn clear_token() -> Result<(), String> {
    match std::fs::remove_file(storage_dir().join("token.json")) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

struct BrowserDelegate;

impl InstalledFlowDelegate for BrowserDelegate {
    fn present_user_url<'a>(
        &'a self,
        url: &'a str,
        _need_code: bool,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            webbrowser::open(url).map_err(|e| e.to_string())?;
            Ok(String::new())
        })
    }
}

#[derive(Clone, Serialize)]
pub struct PlaylistSummary {
    pub id: String,
    pub title: String,
    pub item_count: usize,
}

#[derive(Clone)]
pub struct YouTube {
    auth: DefaultAuthenticator,
    http: reqwest::Client,
    token_path: PathBuf,
}

/// 書き戻しの結果。途中で失敗した場合は done 件まで反映済み。
pub struct WriteOutcome {
    pub done: usize,
    pub error: Option<String>,
}

impl YouTube {
    pub async fn connect() -> Result<Self, String> {
        let dir = base_dir()?;
        let secret = yup_oauth2::read_application_secret(dir.join("client_secret.json"))
            .await
            .map_err(|e| e.to_string())?;
        let auth = InstalledFlowAuthenticator::builder(secret, InstalledFlowReturnMethod::HTTPRedirect)
            .persist_tokens_to_disk(dir.join("token.json")) // 次回以降は Silent Refresh
            .flow_delegate(Box::new(BrowserDelegate))
            .build()
            .await
            .map_err(|e| e.to_string())?;
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { auth, http, token_path: dir.join("token.json") })
    }

    async fn bearer(&self) -> Result<String, String> {
        // トークン未保存なら初回認可（ブラウザ操作待ち）なので長め、保存済みなら更新通信だけなので短め
        let (limit, what) = if self.token_path.exists() {
            (REFRESH_TIMEOUT, "ログイン情報の更新がタイムアウトしました。ネットワーク接続を確認してください。")
        } else {
            (AUTH_TIMEOUT, "ブラウザでの認可が完了しませんでした（タイムアウト）。もう一度お試しください。")
        };
        let tok = tokio::time::timeout(limit, self.auth.token(&[SCOPE]))
            .await
            .map_err(|_| what.to_string())?
            .map_err(|e| e.to_string())?;
        tok.token().map(str::to_string).ok_or_else(|| "アクセストークンを取得できません".into())
    }

    async fn check(resp: reqwest::Response) -> Result<Value, String> {
        let status = resp.status();
        let body: Value = resp.json().await.map_err(net_err)?;
        if status.is_success() {
            Ok(body)
        } else {
            let msg = body["error"]["message"].as_str().unwrap_or("unknown error");
            Err(format!("YouTube API {status}: {msg}"))
        }
    }

    async fn get_json(&self, url: &str, query: &[(&str, &str)]) -> Result<Value, String> {
        let resp = self
            .http
            .get(url)
            .bearer_auth(self.bearer().await?)
            .query(query)
            .send()
            .await
            .map_err(net_err)?;
        Self::check(resp).await
    }

    /// playlists.list（50件/ページ）。mine=true なら自分のプレイリスト全件、id 指定ならその1件。
    async fn fetch_summaries(&self, filter: (&str, &str)) -> Result<Vec<PlaylistSummary>, String> {
        let mut out = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut query = vec![("part", "snippet,contentDetails"), ("maxResults", "50"), filter];
            if let Some(t) = &page_token {
                query.push(("pageToken", t));
            }
            let body = self.get_json(PLAYLISTS, &query).await?;
            for it in body["items"].as_array().into_iter().flatten() {
                out.push(PlaylistSummary {
                    id: it["id"].as_str().unwrap_or_default().into(),
                    title: it["snippet"]["title"].as_str().unwrap_or_default().into(),
                    item_count: it["contentDetails"]["itemCount"].as_u64().unwrap_or(0) as usize,
                });
            }
            page_token = body["nextPageToken"].as_str().map(str::to_string);
            if page_token.is_none() {
                break;
            }
        }
        Ok(out)
    }

    pub async fn my_playlists(&self) -> Result<Vec<PlaylistSummary>, String> {
        self.fetch_summaries(("mine", "true")).await
    }

    pub async fn playlist_summary(&self, id: &str) -> Result<PlaylistSummary, String> {
        self.fetch_summaries(("id", id))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| format!("プレイリストが見つかりません: {id}"))
    }

    /// playlistItems.list（50件/ページ）で全件取得。
    pub async fn fetch_playlist(&self, playlist_id: &str, title: &str) -> Result<Playlist, String> {
        let mut items = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut query = vec![
                ("part", "snippet"),
                ("playlistId", playlist_id),
                ("maxResults", "50"),
            ];
            if let Some(t) = &page_token {
                query.push(("pageToken", t));
            }
            let body = self.get_json(API, &query).await?;
            for it in body["items"].as_array().into_iter().flatten() {
                let sn = &it["snippet"];
                let pos = sn["position"].as_u64().unwrap_or(0) as usize;
                items.push(PlaylistItem {
                    playlist_item_id: it["id"].as_str().unwrap_or_default().into(),
                    video_id: sn["resourceId"]["videoId"].as_str().unwrap_or_default().into(),
                    title: sn["title"].as_str().unwrap_or_default().into(),
                    original_position: pos,
                    current_position: pos,
                    thumbnail_url: best_thumbnail(&sn["thumbnails"]),
                });
            }
            page_token = body["nextPageToken"].as_str().map(str::to_string);
            if page_token.is_none() {
                break;
            }
        }
        Ok(Playlist::new(playlist_id.to_string(), title.to_string(), items))
    }

    /// 差分を playlistItems.update で順に反映する。(playlist_item_id, video_id, 新position)
    pub async fn write_back(&self, playlist_id: &str, updates: Vec<(String, String, usize)>) -> WriteOutcome {
        let mut done = 0;
        for (id, video_id, position) in &updates {
            if let Err(error) = self.update_one(playlist_id, id, video_id, *position).await {
                return WriteOutcome { done, error: Some(error) };
            }
            done += 1;
        }
        WriteOutcome { done, error: None }
    }

    async fn update_one(&self, playlist_id: &str, id: &str, video_id: &str, position: usize) -> Result<(), String> {
        let body = json!({
            "id": id,
            "snippet": {
                "playlistId": playlist_id,
                "resourceId": { "kind": "youtube#video", "videoId": video_id },
                "position": position,
            }
        });
        let resp = self
            .http
            .put(API)
            .bearer_auth(self.bearer().await?)
            .query(&[("part", "snippet")])
            .json(&body)
            .send()
            .await
            .map_err(net_err)?;
        Self::check(resp).await.map(|_| ())
    }
}

fn best_thumbnail(thumbs: &Value) -> String {
    ["maxres", "standard", "high", "medium", "default"]
        .iter()
        .find_map(|k| thumbs[k]["url"].as_str())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::parse_playlist_id;

    #[test]
    fn parses_url_and_id() {
        assert_eq!(parse_playlist_id("https://www.youtube.com/playlist?list=PLabc_-1&x=2"), "PLabc_-1");
        assert_eq!(parse_playlist_id(" PLxyz "), "PLxyz");
    }
}
