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
    fn upsert_summary(&mut self, s: PlaylistSummary) {
        match self.summaries.iter_mut().find(|x| x.id == s.id) {
            Some(x) => *x = s,
            None => self.summaries.push(s),
        }
    }
}

type App<'a> = State<'a, Mutex<Inner>>;

/// 接続（クライアント生成）を直列化するためのロック。通信中の共有状態ロックとは別にしてある。
#[derive(Default)]
struct ConnectLock(Mutex<()>);

/// 接続済みクライアントを返す（未接続なら生成）。共有状態のロックは通信中に保持しない。
async fn client(app: &App<'_>, conn: &State<'_, ConnectLock>) -> Result<YouTube, String> {
    let _serial = conn.0.lock().await;
    let existing = app.lock().await.client.clone();
    if let Some(c) = existing {
        return Ok(c);
    }
    let c = YouTube::connect().await?;
    app.lock().await.client = Some(c.clone());
    Ok(c)
}

/// 並べ替え・書き戻しの結果を他ウィンドウへ通知する。
fn notify(handle: &AppHandle, playlist_id: &str) {
    let _ = handle.emit("playlist-changed", playlist_id.to_string());
}

/// 自分のプレイリスト一覧を取得（playlists.list: 1ユニット/50件）。手動追加分は保持する。
#[tauri::command]
async fn list_my_playlists(app: App<'_>, conn: State<'_, ConnectLock>) -> Result<Vec<PlaylistSummary>, String> {
    let c = client(&app, &conn).await?;
    let mine = c.my_playlists().await?; // 通信中は共有状態をロックしない
    let mut g = app.lock().await;
    for s in mine {
        g.upsert_summary(s);
    }
    Ok(g.summaries.clone())
}

/// URL または ID でプレイリストを一覧へ追加。
#[tauri::command]
async fn add_playlist(
    app: App<'_>,
    conn: State<'_, ConnectLock>,
    playlist_input: String,
) -> Result<Vec<PlaylistSummary>, String> {
    let id = youtube::parse_playlist_id(&playlist_input);
    let c = client(&app, &conn).await?;
    let s = c.playlist_summary(&id).await?;
    let mut g = app.lock().await;
    g.upsert_summary(s);
    Ok(g.summaries.clone())
}

/// 動画一覧を返す。未取得なら playlistItems.list で取得（1ユニット/50件）、取得済みならキャッシュ。
#[tauri::command]
async fn get_playlist(app: App<'_>, conn: State<'_, ConnectLock>, playlist_id: String) -> Result<Playlist, String> {
    let title = {
        let g = app.lock().await;
        if let Some(p) = g.playlists.get(&playlist_id) {
            return Ok(p.clone());
        }
        g.summaries
            .iter()
            .find(|s| s.id == playlist_id)
            .map(|s| s.title.clone())
            .unwrap_or_default()
    };
    let c = client(&app, &conn).await?;
    let pl = c.fetch_playlist(&playlist_id, &title).await?; // 通信中は共有状態をロックしない
    let mut g = app.lock().await;
    Ok(g.playlists.entry(playlist_id).or_insert(pl).clone())
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
struct PendingPlaylist {
    id: String,
    title: String,
    count: usize,
}

/// YouTube へ未反映の変更があるプレイリストと、その更新件数（Push 対象）。
#[tauri::command]
async fn pending_summary(app: App<'_>) -> Result<Vec<PendingPlaylist>, String> {
    let g = app.lock().await;
    let mut out: Vec<PendingPlaylist> = g
        .playlists
        .values()
        .filter_map(|p| {
            let count = p.pending_updates().len();
            (count > 0).then(|| PendingPlaylist { id: p.playlist_id.clone(), title: p.title.clone(), count })
        })
        .collect();
    out.sort_by(|a, b| a.title.cmp(&b.title));
    Ok(out)
}

#[derive(Serialize)]
struct WriteResult {
    updated: usize,
    state: Playlist,
}

/// playlistItems.update で差分のみ書き戻し（1件50ユニット）。通信中は共有状態をロックしない。
#[tauri::command]
async fn write_back(app: App<'_>, handle: AppHandle, playlist_id: String) -> Result<WriteResult, String> {
    let (client, updates, order) = {
        let g = app.lock().await;
        let client = g.client.clone().ok_or("先にプレイリストを読み込んでください")?;
        let p = g.playlists.get(&playlist_id).ok_or("プレイリストが読み込まれていません")?;
        let updates: Vec<(String, String, usize)> = p
            .pending_updates()
            .into_iter()
            .map(|(i, pos)| (i.playlist_item_id.clone(), i.video_id.clone(), pos))
            .collect();
        let order: Vec<String> = p.items.iter().map(|i| i.playlist_item_id.clone()).collect();
        (client, updates, order)
    };
    let outcome = client.write_back(&playlist_id, updates).await;
    let mut g = app.lock().await;
    if let Some(e) = outcome.error {
        if outcome.done > 0 {
            // 一部だけ反映済み: サーバーの並びが不明なのでキャッシュを破棄し、次回取得し直す
            g.playlists.remove(&playlist_id);
            drop(g);
            notify(&handle, &playlist_id);
            return Err(format!("{e}\n（{}件まで反映済み。最新の並びを取得し直します）", outcome.done));
        }
        return Err(e);
    }
    let p = g.playlists.get_mut(&playlist_id).ok_or("プレイリストが読み込まれていません")?;
    p.commit_order(&order);
    let state = p.clone();
    drop(g);
    notify(&handle, &playlist_id);
    Ok(WriteResult { updated: outcome.done, state })
}

/// 一覧からプレイリストを外す（このアプリ内の管理のみ。YouTube 上のプレイリストは削除しない）。
#[tauri::command]
async fn remove_playlist(app: App<'_>, handle: AppHandle, playlist_id: String) -> Result<Vec<PlaylistSummary>, String> {
    let list = {
        let mut g = app.lock().await;
        g.summaries.retain(|s| s.id != playlist_id);
        g.playlists.remove(&playlist_id);
        g.summaries.clone()
    };
    // 開いている並べ替えウィンドウがあれば閉じる
    let safe: String = playlist_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if let Some(w) = handle.get_webview_window(&format!("editor-{safe}")) {
        let _ = w.close();
    }
    Ok(list)
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
struct CredentialsStatus {
    configured: bool,
    dir: String,
}

fn credentials_status_now() -> CredentialsStatus {
    CredentialsStatus {
        configured: youtube::credentials_configured(),
        dir: youtube::storage_dir().display().to_string(),
    }
}

#[tauri::command]
fn credentials_status() -> CredentialsStatus {
    credentials_status_now()
}

/// クライアントID/シークレットを保存。接続済みクライアントは破棄し、次回アクセス時に再認可する。
#[tauri::command]
async fn save_credentials(app: App<'_>, client_id: String, client_secret: String) -> Result<CredentialsStatus, String> {
    youtube::save_credentials(&client_id, &client_secret)?;
    app.lock().await.client = None;
    Ok(credentials_status_now())
}

/// 保存済みの認可（token.json）を削除。
#[tauri::command]
async fn clear_auth(app: App<'_>) -> Result<(), String> {
    youtube::clear_token()?;
    app.lock().await.client = None;
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
        .manage(ConnectLock::default())
        .invoke_handler(tauri::generate_handler![
            list_my_playlists,
            add_playlist,
            get_playlist,
            sort_title,
            shuffle,
            move_item,
            reset,
            pending_count,
            pending_summary,
            write_back,
            open_editor,
            remove_playlist,
            credentials_status,
            save_credentials,
            clear_auth,
            app_info,
            open_url
        ])
        .run(tauri::generate_context!())
        .expect("failed to run app");
}
