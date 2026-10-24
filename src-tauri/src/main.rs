#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod model;
mod youtube;

use model::Playlist;
use serde::Serialize;
use tauri::State;
use tokio::sync::Mutex;
use youtube::YouTube;

#[derive(Default)]
struct Inner {
    client: Option<YouTube>,
    playlist: Option<Playlist>,
}

type App<'a> = State<'a, Mutex<Inner>>;

#[derive(Serialize)]
struct WriteResult<'a> {
    updated: usize,
    state: &'a Playlist,
}

#[tauri::command]
async fn load(app: App<'_>, playlist_input: String) -> Result<Playlist, String> {
    let mut g = app.lock().await;
    if g.client.is_none() {
        g.client = Some(YouTube::connect().await?); // 初回はブラウザで認可
    }
    let pl = g.client.as_ref().unwrap().fetch_playlist(&youtube::parse_playlist_id(&playlist_input)).await?;
    g.playlist = Some(pl);
    Ok(g.playlist.clone().unwrap())
}

async fn edit(app: &App<'_>, op: impl FnOnce(&mut Playlist) -> Result<(), String>) -> Result<Playlist, String> {
    let mut g = app.lock().await;
    let p = g.playlist.as_mut().ok_or("プレイリストが読み込まれていません")?;
    op(p)?;
    Ok(p.clone())
}

#[tauri::command]
async fn sort_title(app: App<'_>, descending: bool) -> Result<Playlist, String> {
    edit(&app, |p| Ok(p.sort_by_title(descending))).await
}

#[tauri::command]
async fn shuffle(app: App<'_>) -> Result<Playlist, String> {
    edit(&app, |p| Ok(p.shuffle())).await
}

#[tauri::command]
async fn move_item(app: App<'_>, from_index: usize, to_index: usize) -> Result<Playlist, String> {
    edit(&app, |p| p.move_item(from_index, to_index)).await
}

#[tauri::command]
async fn reset(app: App<'_>) -> Result<Playlist, String> {
    edit(&app, |p| Ok(p.reset())).await
}

#[tauri::command]
async fn pending_count(app: App<'_>) -> Result<usize, String> {
    Ok(app.lock().await.playlist.as_ref().map_or(0, |p| p.pending_updates().len()))
}

/// playlistItems.update で差分のみ書き戻し（1件50ユニット）。
#[tauri::command]
async fn write_back(app: App<'_>) -> Result<serde_json::Value, String> {
    let mut g = app.lock().await;
    let Inner { client, playlist } = &mut *g;
    let client = client.as_ref().ok_or("先にプレイリストを読み込んでください")?;
    let playlist = playlist.as_mut().ok_or("プレイリストが読み込まれていません")?;
    let updated = client.write_back(playlist).await?;
    serde_json::to_value(WriteResult { updated, state: playlist }).map_err(|e| e.to_string())
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
            load, sort_title, shuffle, move_item, reset, pending_count, write_back, app_info, open_url
        ])
        .run(tauri::generate_context!())
        .expect("failed to run app");
}
