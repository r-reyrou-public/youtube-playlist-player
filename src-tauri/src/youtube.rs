//! YouTube Data API v3 連携（OAuth 2.0 Installed App Flow）。

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use serde_json::{json, Value};
use yup_oauth2::authenticator::DefaultAuthenticator;
use yup_oauth2::authenticator_delegate::InstalledFlowDelegate;
use yup_oauth2::{InstalledFlowAuthenticator, InstalledFlowReturnMethod};

use crate::model::{Playlist, PlaylistItem};

const SCOPE: &str = "https://www.googleapis.com/auth/youtube";
const API: &str = "https://www.googleapis.com/youtube/v3/playlistItems";

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

pub struct YouTube {
    auth: DefaultAuthenticator,
    http: reqwest::Client,
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
        Ok(Self { auth, http: reqwest::Client::new() })
    }

    async fn bearer(&self) -> Result<String, String> {
        let tok = self.auth.token(&[SCOPE]).await.map_err(|e| e.to_string())?;
        tok.token().map(str::to_string).ok_or_else(|| "アクセストークンを取得できません".into())
    }

    async fn check(resp: reqwest::Response) -> Result<Value, String> {
        let status = resp.status();
        let body: Value = resp.json().await.map_err(|e| e.to_string())?;
        if status.is_success() {
            Ok(body)
        } else {
            let msg = body["error"]["message"].as_str().unwrap_or("unknown error");
            Err(format!("YouTube API {status}: {msg}"))
        }
    }

    /// playlistItems.list（50件/ページ）で全件取得。
    pub async fn fetch_playlist(&self, playlist_id: &str) -> Result<Playlist, String> {
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
            let resp = self
                .http
                .get(API)
                .bearer_auth(self.bearer().await?)
                .query(&query)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let body = Self::check(resp).await?;
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
        Ok(Playlist::new(playlist_id.to_string(), items))
    }

    /// 差分のみ playlistItems.update で更新。更新件数を返す。
    pub async fn write_back(&self, playlist: &mut Playlist) -> Result<usize, String> {
        let updates: Vec<(String, String, usize)> = playlist
            .pending_updates()
            .into_iter()
            .map(|(i, p)| (i.playlist_item_id.clone(), i.video_id.clone(), p))
            .collect();
        for (id, video_id, position) in &updates {
            let body = json!({
                "id": id,
                "snippet": {
                    "playlistId": playlist.playlist_id,
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
                .map_err(|e| e.to_string())?;
            Self::check(resp).await?;
        }
        playlist.commit();
        Ok(updates.len())
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
