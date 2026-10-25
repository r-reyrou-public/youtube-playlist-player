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

use crate::model::{Playlist, PlaylistItem, VideoInfo};

const SCOPE: &str = "https://www.googleapis.com/auth/youtube";
const API: &str = "https://www.googleapis.com/youtube/v3/playlistItems";
const CHANNELS: &str = "https://www.googleapis.com/youtube/v3/channels";
const PLAYLISTS: &str = "https://www.googleapis.com/youtube/v3/playlists";
const VIDEOS: &str = "https://www.googleapis.com/youtube/v3/videos";

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
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ChannelQuery {
    Id(String),
    Handle(String),
    Username(String),
}

/// URL やハンドル名からチャンネルの検索クエリを取り出す。
pub fn parse_channel_input(input: &str) -> ChannelQuery {
    let s = input.trim();
    let no_proto = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let path = if let Some(rest) = no_proto.strip_prefix("www.youtube.com/") {
        rest
    } else if let Some(rest) = no_proto.strip_prefix("youtube.com/") {
        rest
    } else if let Some(rest) = no_proto.strip_prefix("m.youtube.com/") {
        rest
    } else if let Some(rest) = no_proto.strip_prefix("youtu.be/") {
        rest
    } else {
        no_proto
    };

    let path = path.split(&['?', '#'][..]).next().unwrap_or(path).trim_matches('/');

    if let Some(rest) = path.strip_prefix("channel/") {
        let id = rest.split('/').next().unwrap_or(rest);
        return ChannelQuery::Id(id.to_string());
    }

    if let Some(rest) = path.strip_prefix("user/") {
        let user = rest.split('/').next().unwrap_or(rest);
        return ChannelQuery::Username(user.to_string());
    }

    if let Some(rest) = path.strip_prefix("c/") {
        let c = rest.split('/').next().unwrap_or(rest);
        return ChannelQuery::Handle(if c.starts_with('@') { c.to_string() } else { format!("@{c}") });
    }

    if let Some(idx) = path.find('@') {
        let rest = &path[idx..];
        let handle = rest.split('/').next().unwrap_or(rest);
        return ChannelQuery::Handle(handle.to_string());
    }

    if (path.starts_with("UC") || path.starts_with("HC")) && path.len() >= 20 && !path.contains('/') {
        return ChannelQuery::Id(path.to_string());
    }

    if s.starts_with('@') {
        let h = s.split(&['/', '?', '#'][..]).next().unwrap_or(s);
        return ChannelQuery::Handle(h.to_string());
    }

    let clean = path.split('/').next().unwrap_or(path);
    ChannelQuery::Handle(format!("@{}", clean.trim_start_matches('@')))
}

#[derive(Clone, Serialize, serde::Deserialize)]
pub struct ChannelDetails {
    pub channel_id: String,
    pub title: String,
    pub description: String,
    pub custom_url: String,
    pub thumbnail_url: String,
    pub uploads_playlist_id: Option<String>,
}

#[derive(Clone, Serialize, serde::Deserialize)]
pub struct ChannelPlaylistsResult {
    pub channel_id: String,
    pub title: String,
    pub handle: String,
    pub thumbnail_url: String,
    pub uploads_playlist_id: Option<String>,
    pub playlists: Vec<PlaylistSummary>,
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
    candidates.extend(app_data_dir());
    candidates
        .into_iter()
        .find(|d| d.join("client_secret.json").exists())
        .ok_or_else(|| {
            "client_secret.json が見つかりません。Google Cloud Console でデスクトップアプリ用\
             OAuthクライアントを作成し、exeと同じフォルダ（またはプロジェクト直下）に配置してください。"
                .to_string()
        })
}

/// exe の隣などが書き込み不可（Program Files 等）の場合の保存先。
fn app_data_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("youtube-playlist-player"))
}

fn writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(format!(".write_test_{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// 認証情報の保存先: 既存の client_secret.json の場所 → 書き込める exe のフォルダ → アプリデータ領域。
pub fn storage_dir() -> PathBuf {
    if let Ok(d) = base_dir() {
        return d;
    }
    if let Some(d) = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from)) {
        if writable(&d) {
            return d;
        }
    }
    let d = app_data_dir().unwrap_or_default();
    let _ = std::fs::create_dir_all(&d);
    d
}

/// ビルド時に埋め込まれた OAuth クライアント（`build.ps1 -Embed` が YT_CLIENT_ID / YT_CLIENT_SECRET を設定）。
/// 利用者が client_secret.json を置いていない場合に使われ、ログインだけで利用できるようになる。
fn embedded_secret() -> Option<yup_oauth2::ApplicationSecret> {
    let id = option_env!("YT_CLIENT_ID").filter(|s| !s.is_empty())?;
    let secret = option_env!("YT_CLIENT_SECRET").filter(|s| !s.is_empty())?;
    Some(yup_oauth2::ApplicationSecret {
        client_id: id.to_string(),
        client_secret: secret.to_string(),
        auth_uri: "https://accounts.google.com/o/oauth2/auth".into(),
        token_uri: "https://oauth2.googleapis.com/token".into(),
        redirect_uris: vec!["http://localhost".into()],
        ..Default::default()
    })
}

/// 利用者の client_secret.json が無く、内蔵クライアントで動作しているか。
pub fn using_embedded() -> bool {
    base_dir().is_err() && embedded_secret().is_some()
}

pub fn credentials_configured() -> bool {
    base_dir().is_ok() || embedded_secret().is_some()
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
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("client_secret.json");
    std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap()).map_err(|e| e.to_string())?;
    // 別のクライアントで取得した古いトークンは使えないため破棄する
    let _ = std::fs::remove_file(dir.join("token.json"));
    Ok(dir)
}

/// Google Cloud Console からダウンロードしたクライアント JSON から (client_id, client_secret) を取り出す。
pub fn parse_client_json(text: &str) -> Result<(String, String), String> {
    let v: Value = serde_json::from_str(text).map_err(|_| "JSON として読み取れません".to_string())?;
    if let Some(c) = v.get("installed") {
        let get = |k: &str| c[k].as_str().map(str::to_string);
        return match (get("client_id"), get("client_secret")) {
            (Some(id), Some(secret)) => Ok((id, secret)),
            _ => Err("client_id / client_secret が含まれていません".into()),
        };
    }
    if v.get("web").is_some() {
        return Err("種類が「ウェブ アプリケーション」です。「デスクトップ アプリ」で作成し直してください".into());
    }
    Err("Google の OAuth クライアント JSON ではありません".into())
}

/// ダウンロード フォルダにある client_secret*.json のうち、取り込める最新のものの内容を返す。
pub fn read_downloaded_client_json() -> Result<String, String> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for var in ["USERPROFILE", "OneDrive"] {
        if let Some(p) = std::env::var_os(var) {
            dirs.push(PathBuf::from(p).join("Downloads"));
        }
    }
    let mut files: Vec<_> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            n.starts_with("client_secret") && n.ends_with(".json")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.is_empty() {
        return Err("ダウンロード フォルダに client_secret*.json が見つかりません".into());
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    let mut first_err = None;
    for (_, path) in files {
        match std::fs::read_to_string(&path) {
            Ok(text) => match parse_client_json(&text) {
                Ok(_) => return Ok(text),
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            },
            Err(e) => {
                first_err.get_or_insert(e.to_string());
            }
        }
    }
    Err(first_err.unwrap_or_default())
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

#[derive(Clone, Serialize, serde::Deserialize)]
pub struct PlaylistSummary {
    pub id: String,
    pub title: String,
    pub item_count: usize,
    /// 手元だけで作成した、まだ YouTube に無いリスト。
    pub is_new: bool,
}

#[derive(Clone)]
pub struct YouTube {
    auth: DefaultAuthenticator,
    http: reqwest::Client,
    token_path: PathBuf,
}

impl YouTube {
    pub async fn connect() -> Result<Self, String> {
        // 利用者の client_secret.json を優先し、無ければ内蔵クライアントを使う
        let (secret, dir) = match base_dir() {
            Ok(dir) => {
                let secret = yup_oauth2::read_application_secret(dir.join("client_secret.json"))
                    .await
                    .map_err(|e| e.to_string())?;
                (secret, dir)
            }
            Err(e) => (embedded_secret().ok_or(e)?, storage_dir()),
        };
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
                    is_new: false,
                });
            }
            page_token = body["nextPageToken"].as_str().map(str::to_string);
            if page_token.is_none() {
                break;
            }
        }
        Ok(out)
    }

    /// 自分のプレイリスト全件。playlists.list には出てこない標準のリスト（高く評価した動画・お気に入り・アップロード）も、
    /// channels.list（1ユニット）の relatedPlaylists から追加する。標準のリストの取得に失敗しても、通常のリストは返す。
    pub async fn my_playlists(&self) -> Result<Vec<PlaylistSummary>, String> {
        let mut out = self.fetch_summaries(("mine", "true")).await?;
        if let Ok(body) = self.get_json(CHANNELS, &[("part", "contentDetails"), ("mine", "true")]).await {
            let related = &body["items"][0]["contentDetails"]["relatedPlaylists"];
            for (key, title) in [("likes", "高く評価した動画"), ("favorites", "お気に入り"), ("uploads", "アップロード動画")] {
                let Some(id) = related[key].as_str().filter(|id| !id.is_empty()) else { continue };
                if out.iter().any(|s| s.id == id) {
                    continue;
                }
                // 一覧に出てこない場合でも、動画の取得（playlistItems）はできるので、既定の名前で追加する
                let found = self.fetch_summaries(("id", id)).await.ok().and_then(|mut v| v.pop());
                let mut s = found.unwrap_or(PlaylistSummary { id: id.to_string(), title: String::new(), item_count: 0, is_new: false });
                if s.title.is_empty() {
                    s.title = title.to_string();
                }
                out.push(s);
            }
        }
        Ok(out)
    }

/// channels.list でチャンネル情報を取得する。
    pub async fn fetch_channel_info(&self, query: &ChannelQuery) -> Result<ChannelDetails, String> {
        let mut items = Vec::new();
        match query {
            ChannelQuery::Id(id) => {
                let body = self.get_json(CHANNELS, &[("part", "snippet,contentDetails"), ("id", id)]).await?;
                if let Some(arr) = body["items"].as_array() {
                    items.extend(arr.iter().cloned());
                }
            }
            ChannelQuery::Handle(handle) => {
                let h = if handle.starts_with('@') { handle.clone() } else { format!("@{handle}") };
                if let Ok(body) = self.get_json(CHANNELS, &[("part", "snippet,contentDetails"), ("forHandle", &h)]).await {
                    if let Some(arr) = body["items"].as_array().filter(|a| !a.is_empty()) {
                        items.extend(arr.iter().cloned());
                    }
                }
                if items.is_empty() {
                    let uname = handle.trim_start_matches('@');
                    if let Ok(body2) = self.get_json(CHANNELS, &[("part", "snippet,contentDetails"), ("forUsername", uname)]).await {
                        if let Some(arr2) = body2["items"].as_array().filter(|a| !a.is_empty()) {
                            items.extend(arr2.iter().cloned());
                        }
                    }
                }
            }
            ChannelQuery::Username(uname) => {
                if let Ok(body) = self.get_json(CHANNELS, &[("part", "snippet,contentDetails"), ("forUsername", uname)]).await {
                    if let Some(arr) = body["items"].as_array().filter(|a| !a.is_empty()) {
                        items.extend(arr.iter().cloned());
                    }
                }
                if items.is_empty() {
                    let h = format!("@{uname}");
                    if let Ok(body) = self.get_json(CHANNELS, &[("part", "snippet,contentDetails"), ("forHandle", &h)]).await {
                        if let Some(arr) = body["items"].as_array() {
                            items.extend(arr.iter().cloned());
                        }
                    }
                }
            }
        }

        let item = items.into_iter().next().ok_or_else(|| "チャンネルが見つかりませんでした。URLやハンドル名をご確認ください。".to_string())?;
        let channel_id = item["id"].as_str().unwrap_or_default().to_string();
        let title = item["snippet"]["title"].as_str().unwrap_or_default().to_string();
        let description = item["snippet"]["description"].as_str().unwrap_or_default().to_string();
        let custom_url = item["snippet"]["customUrl"].as_str().unwrap_or_default().to_string();
        let thumbnail_url = best_thumbnail(&item["snippet"]["thumbnails"]);
        let uploads_playlist_id = item["contentDetails"]["relatedPlaylists"]["uploads"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        Ok(ChannelDetails {
            channel_id,
            title,
            description,
            custom_url,
            thumbnail_url,
            uploads_playlist_id,
        })
    }

    /// 対象チャンネルの公開再生リスト（およびアップロード動画）一覧を取得する。
    /// 動画IDから、その動画のチャンネルIDを取得する。
    pub async fn video_channel(&self, video_id: &str) -> Result<String, String> {
        let query = [
            ("part", "snippet"),
            ("id", video_id),
        ];
        let body = self.get_json(VIDEOS, &query).await?;
        let ch_id = body["items"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|it| it["snippet"]["channelId"].as_str())
            .ok_or_else(|| "動画のチャンネル情報が見つかりませんでした".to_string())?;
        Ok(ch_id.to_string())
    }

    pub async fn channel_playlists(&self, input: &str) -> Result<ChannelPlaylistsResult, String> {
        let query = parse_channel_input(input);
        let channel = self.fetch_channel_info(&query).await?;

        let mut playlists = self.fetch_summaries(("channelId", &channel.channel_id)).await?;

        if let Some(uploads_id) = &channel.uploads_playlist_id {
            if !playlists.iter().any(|p| p.id == *uploads_id) {
                let found = self.fetch_summaries(("id", uploads_id)).await.ok().and_then(|mut v| v.pop());
                let mut s = found.unwrap_or(PlaylistSummary {
                    id: uploads_id.clone(),
                    title: "アップロード動画（すべての動画）".to_string(),
                    item_count: 0,
                    is_new: false,
                });
                if s.title.is_empty() {
                    s.title = "アップロード動画（すべての動画）".to_string();
                }
                playlists.insert(0, s);
            }
        }

        Ok(ChannelPlaylistsResult {
            channel_id: channel.channel_id,
            title: channel.title,
            handle: channel.custom_url,
            thumbnail_url: channel.thumbnail_url,
            uploads_playlist_id: channel.uploads_playlist_id,
            playlists,
        })
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
                let ch_id = sn["videoOwnerChannelId"]
                    .as_str()
                    .unwrap_or_else(|| sn["channelId"].as_str().unwrap_or_default());
                items.push(PlaylistItem {
                    playlist_item_id: it["id"].as_str().unwrap_or_default().into(),
                    video_id: sn["resourceId"]["videoId"].as_str().unwrap_or_default().into(),
                    title: sn["title"].as_str().unwrap_or_default().into(),
                    original_position: pos,
                    current_position: pos,
                    thumbnail_url: best_thumbnail(&sn["thumbnails"]),
                    channel: sn["videoOwnerChannelTitle"].as_str().unwrap_or_default().into(),
                    channel_id: ch_id.to_string(),
                    is_new: false,
                });
            }
            page_token = body["nextPageToken"].as_str().map(str::to_string);
            if page_token.is_none() {
                break;
            }
        }
        Ok(Playlist::new(playlist_id.to_string(), title.to_string(), items))
    }

    async fn send_json(&self, req: reqwest::RequestBuilder, body: &Value) -> Result<Value, String> {
        let resp = req.bearer_auth(self.bearer().await?).json(body).send().await.map_err(net_err)?;
        Self::check(resp).await
    }

    /// playlists.insert: 新しいリストを非公開で作成し、その ID を返す。
    pub async fn create_playlist(&self, title: &str) -> Result<String, String> {
        let body = json!({ "snippet": { "title": title }, "status": { "privacyStatus": "private" } });
        let req = self.http.post(PLAYLISTS).query(&[("part", "snippet,status")]);
        let v = self.send_json(req, &body).await?;
        v["id"].as_str().map(str::to_string).ok_or_else(|| "作成したリストの ID を取得できません".into())
    }

    /// playlistItems.insert: 指定位置に動画を追加し、できた playlistItem の ID を返す。
    pub async fn insert_item(&self, playlist_id: &str, video_id: &str, position: Option<usize>) -> Result<String, String> {
        let mut snippet = json!({
            "playlistId": playlist_id,
            "resourceId": { "kind": "youtube#video", "videoId": video_id },
        });
        if let Some(p) = position {
            snippet["position"] = json!(p); // 並び順が「手動」でないリストは、位置の指定を受け付けない
        }
        let body = json!({ "snippet": snippet });
        let req = self.http.post(API).query(&[("part", "snippet")]);
        let v = self.send_json(req, &body).await?;
        v["id"].as_str().map(str::to_string).ok_or_else(|| "追加した動画の ID を取得できません".into())
    }

    /// playlists.delete: リストそのものを削除する。
    pub async fn delete_playlist(&self, playlist_id: &str) -> Result<(), String> {
        let resp = self
            .http
            .delete(PLAYLISTS)
            .bearer_auth(self.bearer().await?)
            .query(&[("id", playlist_id)])
            .send()
            .await
            .map_err(net_err)?;
        if resp.status().is_success() {
            return Ok(());
        }
        Self::check(resp).await.map(|_| ())
    }

    /// playlistItems.delete: リストから動画を取り除く（動画そのものは削除されない）。
    pub async fn delete_item(&self, item_id: &str) -> Result<(), String> {
        let resp = self
            .http
            .delete(API)
            .bearer_auth(self.bearer().await?)
            .query(&[("id", item_id)])
            .send()
            .await
            .map_err(net_err)?;
        if resp.status().is_success() {
            return Ok(());
        }
        Self::check(resp).await.map(|_| ())
    }

    /// playlistItems.update: 動画の位置を変更する。
    pub async fn move_item(&self, playlist_id: &str, id: &str, video_id: &str, position: usize) -> Result<(), String> {
        let body = json!({
            "id": id,
            "snippet": {
                "playlistId": playlist_id,
                "resourceId": { "kind": "youtube#video", "videoId": video_id },
                "position": position,
            }
        });
        let req = self.http.put(API).query(&[("part", "snippet")]);
        self.send_json(req, &body).await.map(|_| ())
    }
}

/// YouTube の動画 ID（11文字の英数字・ハイフン・アンダースコア）か。
fn valid_video_id(id: &str) -> bool {
    id.len() == 11 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// oEmbed で動画のタイトルとサムネイルを取得する。API キーもログインも不要で、API のクォータを消費しない。
pub async fn fetch_oembed(video_id: &str) -> Result<VideoInfo, String> {
    if !valid_video_id(video_id) {
        return Err("動画IDの形式が正しくありません".into());
    }
    let http = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let watch = format!("https://www.youtube.com/watch?v={video_id}");
    let resp = http
        .get("https://www.youtube.com/oembed")
        .query(&[("url", watch.as_str()), ("format", "json")])
        .send()
        .await
        .map_err(net_err)?;
    let status = resp.status();
    match status.as_u16() {
        200 => {}
        401 | 403 => return Err("この動画の情報を取得できません（非公開・限定公開、または埋め込みが禁止されている動画です）".into()),
        404 => return Err("動画が見つかりません".into()),
        _ => return Err(format!("動画情報の取得に失敗しました（HTTP {status}）")),
    }
    let v: Value = resp.json().await.map_err(net_err)?;
    Ok(VideoInfo {
        video_id: video_id.to_string(),
        title: v["title"].as_str().unwrap_or(video_id).to_string(),
        thumbnail_url: v["thumbnail_url"].as_str().unwrap_or_default().to_string(),
        channel: v["author_name"].as_str().unwrap_or_default().to_string(),
        channel_url: v["author_url"].as_str().unwrap_or_default().to_string(),
    })
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
    use super::{parse_client_json, parse_playlist_id, valid_video_id};

    #[test]
    fn video_id_validation() {
        assert!(valid_video_id("dQw4w9WgXcQ"));
        assert!(valid_video_id("a-b_c-d_e-f"));
        assert!(!valid_video_id("dQw4w9WgXc"));      // 短い
        assert!(!valid_video_id("dQw4w9WgXcQ&x=1")); // URL の断片
        assert!(!valid_video_id("dQw4w9WgX/Q"));
    }

    #[test]
    fn parses_desktop_client_json() {
        let j = r#"{"installed":{"client_id":"a.apps.googleusercontent.com","client_secret":"GOCSPX-x"}}"#;
        assert_eq!(parse_client_json(j).unwrap(), ("a.apps.googleusercontent.com".into(), "GOCSPX-x".into()));
        assert!(parse_client_json(r#"{"web":{"client_id":"a"}}"#).unwrap_err().contains("ウェブ"));
        assert!(parse_client_json("{}").is_err());
        assert!(parse_client_json("not json").is_err());
    }

    #[test]
    fn parses_channel_query() {
        use super::{parse_channel_input, ChannelQuery};
        assert_eq!(parse_channel_input("https://www.youtube.com/@Ado1024"), ChannelQuery::Handle("@Ado1024".into()));
        assert_eq!(parse_channel_input("https://www.youtube.com/@Ado1024/playlists"), ChannelQuery::Handle("@Ado1024".into()));
        assert_eq!(parse_channel_input("https://www.youtube.com/channel/UC2XdaAVUannpujzv32jcouQ"), ChannelQuery::Id("UC2XdaAVUannpujzv32jcouQ".into()));
        assert_eq!(parse_channel_input("UC2XdaAVUannpujzv32jcouQ"), ChannelQuery::Id("UC2XdaAVUannpujzv32jcouQ".into()));
        assert_eq!(parse_channel_input("https://www.youtube.com/user/officialbeatles"), ChannelQuery::Username("officialbeatles".into()));
        assert_eq!(parse_channel_input("@Ado1024"), ChannelQuery::Handle("@Ado1024".into()));
        assert_eq!(parse_channel_input("Ado1024"), ChannelQuery::Handle("@Ado1024".into()));
    }

    #[test]
    fn parses_url_and_id() {
        assert_eq!(parse_playlist_id("https://www.youtube.com/playlist?list=PLabc_-1&x=2"), "PLabc_-1");
        assert_eq!(parse_playlist_id(" PLxyz "), "PLxyz");
    }
}
