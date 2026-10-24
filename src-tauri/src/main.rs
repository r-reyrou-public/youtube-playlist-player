#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod model;
mod youtube;

use std::collections::HashMap;

use model::Playlist;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::Mutex;
use youtube::{PlaylistSummary, YouTube};

#[derive(Default)]
struct Inner {
    client: Option<YouTube>,
    summaries: Vec<PlaylistSummary>,
    playlists: HashMap<String, Playlist>,
}

impl Inner {
    async fn client(&mut self) -> Result<&YouTube, String> {
        if self.client.is_none() {
            self.client = Some(YouTube::connect().await?); // 初回はブラウザで認可
        }
        Ok(self.client.as_ref().unwrap())
    }

    fn upsert_summary(&mut self, s: PlaylistSummary) {
        match self.summaries.iter_mut().find(|x| x.id == s.id) {
            Some(x) => *x = s,
            None => self.summaries.push(s),
        }
    }
}

type App<'a> = State<'a, Mutex<Inner>>;

/// 並べ替え・書き戻しの結果を他ウィンドウへ通知する。
fn notify(handle: &AppHandle, playlist_id: &str) {
    let _ = handle.emit("playlist-changed", playlist_id.to_string());
}

/// 自分のプレイリスト一覧を取得（playlists.list: 1ユニット/50件）。手動追加分は保持する。
#[tauri::command]
async fn list_my_playlists(app: App<'_>) -> Result<Vec<PlaylistSummary>, String> {
    let mut g = app.lock().await;
    let mine = g.client().await?.my_playlists().await?;
    for s in mine {
        g.upsert_summary(s);
    }
    Ok(g.summaries.clone())
}

/// URL または ID でプレイリストを一覧へ追加。
#[tauri::command]
async fn add_playlist(app: App<'_>, playlist_input: String) -> Result<Vec<PlaylistSummary>, String> {
    let id = youtube::parse_playlist_id(&playlist_input);
    let mut g = app.lock().await;
    let s = g.client().await?.playlist_summary(&id).await?;
    g.upsert_summary(s);
    Ok(g.summaries.clone())
}

/// 動画一覧を返す。未取得なら playlistItems.list で取得（1ユニット/50件）、取得済みならキャッシュ。
#[tauri::command]
async fn get_playlist(app: App<'_>, playlist_id: String) -> Result<Playlist, String> {
    let mut g = app.lock().await;
    if !g.playlists.contains_key(&playlist_id) {
        let title = g
            .summaries
            .iter()
            .find(|s| s.id == playlist_id)
            .map(|s| s.title.clone())
            .unwrap_or_default();
        let pl = g.client().await?.fetch_playlist(&playlist_id, &title).await?;
        g.playlists.insert(playlist_id.clone(), pl);
    }
    Ok(g.playlists[&playlist_id].clone())
}

async fn edit(
    app: &App<'_>,
    handle: &AppHandle,
    playlist_id: &str,
    op: impl FnOnce(&mut Playlist) -> Result<(), String>,
) -> Result<Playlist, String> {
    let mut g = app.lock().await;
    let p = g
        .playlists
        .get_mut(playlist_id)
        .ok_or("プレイリストが読み込まれていません")?;
    op(p)?;
    let out = p.clone();
    notify(handle, playlist_id);
    Ok(out)
}

#[tauri::command]
async fn sort_title(app: App<'_>, handle: AppHandle, playlist_id: String, descending: bool) -> Result<Playlist, String> {
    edit(&app, &handle, &playlist_id, |p| Ok(p.sort_by_title(descending))).await
}

#[tauri::command]
async fn shuffle(app: App<'_>, handle: AppHandle, playlist_id: String) -> Result<Playlist, String> {
    edit(&app, &handle, &playlist_id, |p| Ok(p.shuffle())).await
}

#[tauri::command]
async fn move_item(
    app: App<'_>,
    handle: AppHandle,
    playlist_id: String,
    from_index: usize,
    to_index: usize,
) -> Result<Playlist, String> {
    edit(&app, &handle, &playlist_id, |p| p.move_item(from_index, to_index)).await
}

#[tauri::command]
async fn reset(app: App<'_>, handle: AppHandle, playlist_id: String) -> Result<Playlist, String> {
    edit(&app, &handle, &playlist_id, |p| Ok(p.reset())).await
}

#[tauri::command]
async fn pending_count(app: App<'_>, playlist_id: String) -> Result<usize, String> {
    Ok(app.lock().await.playlists.get(&playlist_id).map_or(0, |p| p.pending_updates().len()))
}

#[derive(Serialize)]
struct WriteResult {
    updated: usize,
    state: Playlist,
}

/// playlistItems.update で差分のみ書き戻し（1件50ユニット）。
#[tauri::command]
async fn write_back(app: App<'_>, handle: AppHandle, playlist_id: String) -> Result<WriteResult, String> {
    let mut g = app.lock().await;
    let Inner { client, playlists, .. } = &mut *g;
    let client = client.as_ref().ok_or("先にプレイリストを読み込んでください")?;
    let playlist = playlists
        .get_mut(&playlist_id)
        .ok_or("プレイリストが読み込まれていません")?;
    let updated = client.write_back(playlist).await?;
    notify(&handle, &playlist_id);
    Ok(WriteResult { updated, state: playlist.clone() })
}

/// 並べ替え専用ウィンドウを開く（既に開いていれば前面へ）。
#[tauri::command]
async fn open_editor(handle: AppHandle, playlist_id: String, title: String) -> Result<(), String> {
    let safe: String = playlist_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let label = format!("editor-{safe}");
    if let Some(w) = handle.get_webview_window(&label) {
        return w.set_focus().map_err(|e| e.to_string());
    }
    WebviewWindowBuilder::new(&handle, &label, WebviewUrl::App(format!("editor.html?id={safe}").into()))
        .title(format!("並べ替え - {title}"))
        .inner_size(680.0, 820.0)
        .use_https_scheme(true)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Serialize)]
struct AppInfo {
    version: &'static str,
    repository: &'static str,
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        repository: env!("CARGO_PKG_REPOSITORY"),
    }
}

/// ヘルプ画面のリンク用。http(s) のみ許可。
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("unsupported url".into());
    }
    webbrowser::open(&url).map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .manage(Mutex::new(Inner::default()))
        .invoke_handler(tauri::generate_handler![
            list_my_playlists,
            add_playlist,
            get_playlist,
            sort_title,
            shuffle,
            move_item,
            reset,
            pending_count,
            write_back,
            open_editor,
            app_info,
            open_url
        ])
        .run(tauri::generate_context!())
        .expect("failed to run app");
}
