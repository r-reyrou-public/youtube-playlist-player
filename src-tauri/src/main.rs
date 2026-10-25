#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod model;
mod quota;
mod winstate;
mod youtube;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use model::{Op, Playlist, VideoInfo};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::Mutex;
use youtube::{PlaylistSummary, YouTube};

#[derive(Default)]
struct Inner {
    client: Option<YouTube>,
    summaries: Vec<PlaylistSummary>,
    playlists: HashMap<String, Playlist>,
    /// 「公開リストを開く」で取得中の他人のリスト（コピー用）。
    peek: Option<Playlist>,
    /// ウィンドウ間ドラッグの移動元（playlist_id, 動画の playlist_item_id 複数）。
    drag: Option<(String, Vec<String>)>,
    next_local: u32,
    next_item: u32,
}

impl Inner {
    fn upsert_summary(&mut self, s: PlaylistSummary) {
        match self.summaries.iter_mut().find(|x| x.id == s.id) {
            Some(x) => *x = s,
            None => self.summaries.push(s),
        }
    }

    /// 一覧の並びを変える: id のリストを before の直前へ移す（before が None なら末尾へ）。
    fn move_summary(&mut self, id: &str, before: Option<&str>) {
        let Some(from) = self.summaries.iter().position(|s| s.id == id) else { return };
        let item = self.summaries.remove(from);
        let at = before.and_then(|b| self.summaries.iter().position(|s| s.id == b)).unwrap_or(self.summaries.len());
        self.summaries.insert(at, item);
    }

    /// 一覧用の要約。読み込み済みのリストは、編集中の件数・タイトル・新規かどうかを反映する。
    fn list_summaries(&self) -> Vec<PlaylistSummary> {
        self.summaries
            .iter()
            .filter(|s| !self.playlists.get(&s.id).is_some_and(|p| p.deleted))
            .map(|s| {
                let mut s = s.clone();
                if let Some(p) = self.playlists.get(&s.id) {
                    s.item_count = p.items.len();
                    s.title = p.title.clone();
                    s.is_new = p.remote_id.is_none();
                }
                s
            })
            .collect()
    }

    fn next_item_id(&mut self) -> String {
        self.next_item += 1;
        format!("new-{}", self.next_item)
    }

    fn next_local_id(&mut self) -> String {
        self.next_local += 1;
        format!("local-{}", self.next_local)
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
    schedule_save(handle);
}

#[derive(Default)]
struct SaveCtl(AtomicBool);

/// 取得済みの内容を cache.json に保存する（短時間の連続変更はまとめて1回）。
fn schedule_save(handle: &AppHandle) {
    if handle.state::<SaveCtl>().0.swap(true, Ordering::SeqCst) {
        return;
    }
    let h = handle.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        h.state::<SaveCtl>().0.store(false, Ordering::SeqCst);
        save_now(&h).await;
    });
}

async fn save_now(handle: &AppHandle) {
    #[derive(Serialize)]
    struct Snapshot<'a> {
        playlists: Vec<&'a Playlist>,
        summaries: &'a [PlaylistSummary],
        next_local: u32,
        next_item: u32,
    }
    let app = handle.state::<Mutex<Inner>>();
    let json = {
        let g = app.lock().await;
        serde_json::to_string(&Snapshot {
            playlists: g.playlists.values().collect(),
            summaries: &g.summaries,
            next_local: g.next_local,
            next_item: g.next_item,
        })
    };
    if let Ok(j) = json {
        quota::save_cache_json(&j);
    }
}

/// 保存済みの内容を状態に取り込む。
fn restore(g: &mut Inner, d: quota::Deferred) {
    g.next_local = g.next_local.max(d.next_local);
    g.next_item = g.next_item.max(d.next_item);
    for s in d.summaries {
        g.upsert_summary(s);
    }
    for p in d.playlists {
        g.playlists.insert(p.playlist_id.clone(), p);
    }
}

/// 自分のプレイリスト一覧を取得（playlists.list: 1ユニット/50件）。手動追加分は保持する。
#[tauri::command]
async fn list_my_playlists(app: App<'_>, conn: State<'_, ConnectLock>, handle: AppHandle) -> Result<Vec<PlaylistSummary>, String> {
    let c = client(&app, &conn).await?;
    let mine = c.my_playlists().await?; // 通信中は共有状態をロックしない
    let mut g = app.lock().await;
    for s in mine {
        if g.playlists.get(&s.id).is_some_and(|p| p.deleted) {
            continue; // 削除を Push するまで、一覧には出さない
        }
        g.upsert_summary(s);
    }
    let out = g.list_summaries();
    drop(g);
    schedule_save(&handle);
    Ok(out)
}

/// URL または ID でプレイリストを一覧へ追加。
#[tauri::command]
async fn add_playlist(
    app: App<'_>,
    conn: State<'_, ConnectLock>,
    handle: AppHandle,
    playlist_input: String,
) -> Result<Vec<PlaylistSummary>, String> {
    let id = youtube::parse_playlist_id(&playlist_input);
    let c = client(&app, &conn).await?;
    let s = c.playlist_summary(&id).await?;
    let mut g = app.lock().await;
    if g.playlists.get(&s.id).is_some_and(|p| p.deleted) {
        g.playlists.remove(&s.id); // 削除の予約を取り消す（内容は必要になったとき取得し直す）
    }
    g.upsert_summary(s);
    let out = g.list_summaries();
    drop(g);
    schedule_save(&handle);
    Ok(out)
}

/// チャンネルURLやハンドル名からチャンネル情報および公開再生リスト一覧を取得する。
#[tauri::command]
async fn fetch_channel_playlists(
    app: App<'_>,
    conn: State<'_, ConnectLock>,
    channel_input: String,
) -> Result<youtube::ChannelPlaylistsResult, String> {
    let c = client(&app, &conn).await?;
    let res = c.channel_playlists(&channel_input).await?;
    let cost = 1 + ((res.playlists.len() + 49) / 50).max(1) as u32;
    quota::add(cost);
    Ok(res)
}

/// 複数のプレイリストを一括で一覧へ追加する。
#[tauri::command]
async fn add_playlists(
    app: App<'_>,
    conn: State<'_, ConnectLock>,
    handle: AppHandle,
    playlist_ids: Vec<String>,
) -> Result<Vec<PlaylistSummary>, String> {
    let c = client(&app, &conn).await?;
    let mut g = app.lock().await;
    for id in playlist_ids {
        if let Ok(s) = c.playlist_summary(&id).await {
            if g.playlists.get(&s.id).is_some_and(|p| p.deleted) {
                g.playlists.remove(&s.id);
            }
            g.upsert_summary(s);
        }
    }
    let out = g.list_summaries();
    drop(g);
    schedule_save(&handle);
    Ok(out)
}

/// 一覧でのリストの並び順を変える（通信なし）。before が None なら末尾へ。並びは cache.json に保存される。
#[tauri::command]
async fn move_summary(app: App<'_>, handle: AppHandle, playlist_id: String, before: Option<String>) -> Result<Vec<PlaylistSummary>, String> {
    let out = {
        let mut g = app.lock().await;
        g.move_summary(&playlist_id, before.as_deref());
        g.list_summaries()
    };
    schedule_save(&handle);
    Ok(out)
}

/// 通信なしで、現在の一覧（編集中の件数・新規リストを含む）を返す。
#[tauri::command]
async fn list_summaries(app: App<'_>) -> Result<Vec<PlaylistSummary>, String> {
    Ok(app.lock().await.list_summaries())
}

/// 新しいリストを手元に作る（YouTube への作成は Push のとき。非公開で作成される）。
#[tauri::command]
async fn create_playlist(app: App<'_>, handle: AppHandle, title: String) -> Result<Vec<PlaylistSummary>, String> {
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("リスト名を入力してください".into());
    }
    if title.chars().count() > 150 {
        return Err("リスト名は150文字以内にしてください".into());
    }
    let (id, list) = {
        let mut g = app.lock().await;
        let id = g.next_local_id();
        g.playlists.insert(id.clone(), Playlist::new_local(id.clone(), title.clone()));
        g.summaries.push(PlaylistSummary { id: id.clone(), title, item_count: 0, is_new: true });
        (id, g.list_summaries())
    };
    notify(&handle, &id);
    Ok(list)
}

/// 動画の保存先リストを変更する（add に入れ、remove から外す）。反映は Push のとき。
/// 外すときは、そのリスト内の同じ動画をすべて取り除く。対象のリストは取得済みである必要がある。
#[tauri::command]
async fn set_video_playlists(
    app: App<'_>,
    handle: AppHandle,
    video: VideoInfo,
    add: Vec<String>,
    remove: Vec<String>,
) -> Result<(), String> {
    let mut changed = Vec::new();
    {
        let mut g = app.lock().await;
        for id in &remove {
            if let Some(p) = g.playlists.get_mut(id) {
                if p.remove_video(&video.video_id) > 0 {
                    changed.push(id.clone());
                }
            }
        }
        for id in &add {
            let new_id = g.next_item_id();
            let p = g.playlists.get_mut(id).ok_or("プレイリストが読み込まれていません")?;
            if !p.contains_video(&video.video_id) {
                p.add_video(video.clone(), new_id, None);
                changed.push(id.clone());
            }
        }
    }
    for id in changed {
        notify(&handle, &id);
    }
    Ok(())
}

#[derive(Serialize)]
struct ImportResult {
    added: usize,
    skipped: usize,
}

/// 別のリストの動画を、target リストの末尾へまとめてコピーする（元のリストは変わらない）。
/// すでに target に入っている動画（同じ動画ID）はスキップする。反映は Push のとき。
/// sources は取得済みである必要がある。
#[tauri::command]
async fn import_playlists(
    app: App<'_>,
    handle: AppHandle,
    target: String,
    sources: Vec<String>,
) -> Result<ImportResult, String> {
    let (added, skipped) = {
        let mut g = app.lock().await;
        if !g.playlists.contains_key(&target) {
            return Err("取り込み先のリストが読み込まれていません".into());
        }
        let mut videos: Vec<VideoInfo> = Vec::new();
        for id in sources.iter().filter(|id| **id != target) {
            let p = g.playlists.get(id).ok_or("取り込み元のリストが読み込まれていません")?;
            videos.extend(p.items.iter().map(|i| VideoInfo {
                video_id: i.video_id.clone(),
                title: i.title.clone(),
                thumbnail_url: i.thumbnail_url.clone(),
                channel: i.channel.clone(),
                channel_url: String::new(),
            }));
        }
        let (mut added, mut skipped) = (0usize, 0usize);
        for v in videos {
            let new_id = g.next_item_id();
            let t = g.playlists.get_mut(&target).ok_or("取り込み先のリストが読み込まれていません")?;
            if t.contains_video(&v.video_id) {
                skipped += 1;
            } else {
                t.add_video(v, new_id, None);
                added += 1;
            }
        }
        (added, skipped)
    };
    notify(&handle, &target);
    Ok(ImportResult { added, skipped })
}

#[derive(Serialize)]
struct PeekItem {
    video_id: String,
    title: String,
    channel: String,
}

#[derive(Serialize)]
struct PeekResult {
    title: String,
    items: Vec<PeekItem>,
}

/// 公開リストの URL / ID から全動画を取得して返す（一覧には追加しない）。取得は playlistItems.list（1ユニット/50件）。
/// 取得した内容は copy_peeked のために保持する。
#[tauri::command]
async fn peek_public_playlist(app: App<'_>, conn: State<'_, ConnectLock>, playlist_input: String) -> Result<PeekResult, String> {
    let src_id = youtube::parse_playlist_id(&playlist_input);
    let c = client(&app, &conn).await?;
    let summary = c.playlist_summary(&src_id).await?;
    let src = c.fetch_playlist(&src_id, &summary.title).await?; // 通信中は共有状態をロックしない
    let out = PeekResult {
        title: summary.title.clone(),
        items: src
            .items
            .iter()
            .map(|i| PeekItem { video_id: i.video_id.clone(), title: i.title.clone(), channel: i.channel.clone() })
            .collect(),
    };
    app.lock().await.peek = Some(src);
    Ok(out)
}

#[derive(Serialize)]
struct CopyResult {
    id: String,
    title: String,
    added: usize,
    skipped: usize,
}

/// peek_public_playlist で開いたリストのうち、選んだ動画（video_ids。元の並び順を保つ）を自分のリストへコピーする。
/// target が None なら、title の新規リスト（Push で非公開リストとして作成）を手元に作る。Some なら、そのリストの末尾に追加する。
#[tauri::command]
async fn copy_peeked(
    app: App<'_>,
    handle: AppHandle,
    video_ids: Vec<String>,
    target: Option<String>,
    title: String,
) -> Result<CopyResult, String> {
    let chosen: HashSet<String> = video_ids.into_iter().collect();
    let (id, title, added, skipped) = {
        let mut g = app.lock().await;
        let src = g.peek.clone().ok_or("コピー元のリストが開かれていません")?;
        let (id, title, mut pl, is_new) = match &target {
            Some(t) => {
                let p = g.playlists.remove(t).ok_or("コピー先のリストが読み込まれていません")?;
                (t.clone(), p.title.clone(), p, false)
            }
            None => {
                let title: String = title.trim().chars().take(150).collect();
                if title.is_empty() {
                    return Err("リスト名を入力してください".into());
                }
                let id = g.next_local_id();
                (id.clone(), title.clone(), Playlist::new_local(id, title), true)
            }
        };
        let (mut added, mut skipped) = (0usize, 0usize);
        for i in src.items.iter().filter(|i| chosen.contains(&i.video_id)) {
            if pl.contains_video(&i.video_id) {
                skipped += 1;
                continue;
            }
            let new_id = g.next_item_id();
            pl.add_video(
                VideoInfo {
                    video_id: i.video_id.clone(),
                    title: i.title.clone(),
                    thumbnail_url: i.thumbnail_url.clone(),
                    channel: i.channel.clone(),
                    channel_url: String::new(),
                },
                new_id,
                None,
            );
            added += 1;
        }
        g.playlists.insert(id.clone(), pl);
        if is_new {
            g.summaries.push(PlaylistSummary { id: id.clone(), title: title.clone(), item_count: added, is_new: true });
        }
        (id, title, added, skipped)
    };
    notify(&handle, &id);
    Ok(CopyResult { id, title, added, skipped })
}

/// 動画1本の情報（タイトル・サムネイル）を oEmbed で取得する。API のクォータは消費せず、ログインも不要。
#[tauri::command]
async fn video_info(video_id: String) -> Result<VideoInfo, String> {
    youtube::fetch_oembed(&video_id).await
}

/// 動画IDから、その動画のチャンネルIDまたはチャンネルURLを取得する。
#[tauri::command]
async fn get_video_channel(
    app: App<'_>,
    conn: State<'_, ConnectLock>,
    video_id: String,
) -> Result<String, String> {
    if let Ok(info) = youtube::fetch_oembed(&video_id).await {
        if !info.channel_url.trim().is_empty() {
            return Ok(info.channel_url);
        }
    }
    let c = client(&app, &conn).await?;
    let ch = c.video_channel(&video_id).await?;
    quota::add(1);
    Ok(ch)
}

/// ウィンドウ間ドラッグの開始: 移動元を覚えておく（ドロップ側のウィンドウが move_between で使う）。
#[tauri::command]
async fn drag_begin(app: App<'_>, playlist_id: String, item_ids: Vec<String>) -> Result<(), String> {
    app.lock().await.drag = Some((playlist_id, item_ids));
    Ok(())
}

/// 別ウィンドウのリストへ動画を移す（元のリストからは取り除く）。反映は Push のとき。
#[tauri::command]
async fn move_between(
    app: App<'_>,
    handle: AppHandle,
    to_playlist: String,
    to_index: usize,
) -> Result<Playlist, String> {
    let (from, out) = {
        let mut g = app.lock().await;
        let (from, item_ids) = g.drag.take().ok_or("移動元の情報がありません。もう一度ドラッグしてください")?;
        if from == to_playlist {
            return Err("同じリスト内の移動は、ウィンドウ内でドラッグしてください".into());
        }
        if !g.playlists.contains_key(&from) || !g.playlists.contains_key(&to_playlist) {
            return Err("プレイリストが読み込まれていません".into());
        }
        // 元のリストでの並び順を保ったまま、すべて取り出す
        let order: Vec<String> = g.playlists[&from]
            .items
            .iter()
            .filter(|i| item_ids.contains(&i.playlist_item_id))
            .map(|i| i.playlist_item_id.clone())
            .collect();
        if order.is_empty() {
            return Err("移動する動画が見つかりません".into());
        }
        let mut moved = Vec::new();
        for id in &order {
            if let Some(item) = g.playlists.get_mut(&from).and_then(|p| p.take_item(id)) {
                moved.push(item);
            }
        }
        for (k, item) in moved.into_iter().enumerate() {
            let new_id = g.next_item_id();
            let target = g.playlists.get_mut(&to_playlist).ok_or("プレイリストが読み込まれていません")?;
            target.add_video(
                VideoInfo { video_id: item.video_id, title: item.title, thumbnail_url: item.thumbnail_url, channel: item.channel, channel_url: String::new() },
                new_id,
                Some(to_index + k),
            );
        }
        let out = g.playlists[&to_playlist].clone();
        (from, out)
    };
    notify(&handle, &from);
    notify(&handle, &to_playlist);
    Ok(out)
}

/// 動画一覧を返す。未取得なら playlistItems.list で取得（1ユニット/50件）、取得済みならキャッシュ。
#[tauri::command]
async fn get_playlist(app: App<'_>, conn: State<'_, ConnectLock>, handle: AppHandle, playlist_id: String) -> Result<Playlist, String> {
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
    let out = g.playlists.entry(playlist_id).or_insert(pl).clone();
    drop(g);
    schedule_save(&handle);
    Ok(out)
}

/// YouTube から最新を取得し直す（手元の未反映の変更は破棄される）。手元だけの新規リストは対象外。
#[tauri::command]
async fn refresh_playlist(app: App<'_>, conn: State<'_, ConnectLock>, handle: AppHandle, playlist_id: String) -> Result<Playlist, String> {
    let (remote, title) = {
        let g = app.lock().await;
        let title = g.summaries.iter().find(|s| s.id == playlist_id).map(|s| s.title.clone()).unwrap_or_default();
        match g.playlists.get(&playlist_id) {
            Some(p) if p.remote_id.is_none() => return Err("まだ YouTube に作成されていない新規リストは、取得し直せません".into()),
            Some(p) => (p.remote_id.clone().unwrap_or_default(), p.title.clone()),
            None => (playlist_id.clone(), title),
        }
    };
    let c = client(&app, &conn).await?;
    let pl = c.fetch_playlist(&remote, &title).await?; // 通信中は共有状態をロックしない
    app.lock().await.playlists.insert(playlist_id.clone(), pl.clone());
    notify(&handle, &playlist_id);
    Ok(pl)
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
async fn sort_items(app: App<'_>, handle: AppHandle, playlist_id: String, key: String, descending: bool) -> Result<Playlist, String> {
    edit(&app, &handle, &playlist_id, |p| Ok(p.sort_by(&key, descending))).await
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

/// リストから動画を取り除く（YouTube からの削除は Push のとき。「元に戻す」で復活できる）。
#[tauri::command]
async fn remove_items(app: App<'_>, handle: AppHandle, playlist_id: String, item_ids: Vec<String>) -> Result<Playlist, String> {
    edit(&app, &handle, &playlist_id, |p| {
        for id in &item_ids {
            p.take_item(id);
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn reset(app: App<'_>, handle: AppHandle, playlist_id: String) -> Result<Playlist, String> {
    edit(&app, &handle, &playlist_id, |p| Ok(p.reset())).await
}

#[tauri::command]
async fn pending_count(app: App<'_>, playlist_id: String) -> Result<usize, String> {
    Ok(app.lock().await.playlists.get(&playlist_id).map_or(0, |p| p.plan().len()))
}

#[derive(Serialize)]
struct PendingPlaylist {
    id: String,
    title: String,
    count: usize,
    /// リストの削除（YouTube からも削除する）。
    deleting: bool,
}

fn pending_lists(g: &Inner, ids: &Option<Vec<String>>) -> Vec<PendingPlaylist> {
    let mut out: Vec<PendingPlaylist> = g
        .playlists
        .values()
        .filter(|p| ids.as_ref().is_none_or(|v| v.contains(&p.playlist_id)))
        .filter_map(|p| {
            let count = p.plan().len();
            (count > 0).then(|| PendingPlaylist { id: p.playlist_id.clone(), title: p.title.clone(), count, deleting: p.deleted })
        })
        .collect();
    out.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)));
    out
}

/// YouTube へ未反映の変更があるプレイリストと、その更新件数（Push 対象）。
#[tauri::command]
async fn pending_summary(app: App<'_>) -> Result<Vec<PendingPlaylist>, String> {
    Ok(pending_lists(&*app.lock().await, &None))
}

#[derive(Serialize)]
struct PushPlan {
    lists: Vec<PendingPlaylist>,
    ops: usize,
    cost: u32,
    limit: u32,
    used: u32,
    remaining: u32,
    /// 今日の残りで全件実行できるか。
    fits: bool,
    /// 今日の残りで実行できる件数。
    affordable_ops: usize,
    /// 全件の実行に必要な日数（今日を含む）。
    days_needed: u32,
    reset_in_secs: i64,
    running: bool,
}

/// Push の前に、対象・件数・概算コスト・今日の残りクォータを返す（通信なし）。playlist_ids が None なら全リスト。
#[tauri::command]
async fn push_plan(app: App<'_>, ctl: State<'_, PushCtl>, playlist_ids: Option<Vec<String>>) -> Result<PushPlan, String> {
    let lists = pending_lists(&*app.lock().await, &playlist_ids);
    let ops: usize = lists.iter().map(|p| p.count).sum();
    let cost = ops as u32 * quota::OP_COST;
    let used = quota::used_today();
    let remaining = quota::DAILY_LIMIT.saturating_sub(used);
    Ok(PushPlan {
        lists,
        ops,
        cost,
        limit: quota::DAILY_LIMIT,
        used,
        remaining,
        fits: cost <= remaining,
        affordable_ops: quota::affordable_ops(),
        days_needed: quota::days_needed(ops),
        reset_in_secs: quota::secs_until_reset(quota::now_secs()),
        running: ctl.running.load(Ordering::SeqCst),
    })
}

#[derive(Default)]
struct PushCtl {
    running: AtomicBool,
    cancel: AtomicBool,
}

#[derive(Serialize, Clone)]
struct PushProgress {
    /// running / waiting / done / deferred / cancelled / error
    state: &'static str,
    done: usize,
    total: usize,
    message: String,
    /// waiting のとき、再開予定の時刻（UNIX 秒）。
    resume_at: i64,
}

fn emit_progress(h: &AppHandle, state: &'static str, done: usize, total: usize, message: String, resume_at: i64) {
    let _ = h.emit("push-progress", PushProgress { state, done, total, message, resume_at });
}

/// 未反映の変更を、別日・次回起動時に回すために保存する（変更が無ければ保存ファイルを消す）。
fn save_deferred(g: &Inner) {
    let playlists: Vec<Playlist> = g.playlists.values().filter(|p| !p.plan().is_empty()).cloned().collect();
    let ids: HashSet<&str> = playlists.iter().map(|p| p.playlist_id.as_str()).collect();
    let summaries = g.summaries.iter().filter(|s| ids.contains(s.id.as_str())).cloned().collect();
    quota::save_deferred(&quota::Deferred { playlists, summaries, next_local: g.next_local, next_item: g.next_item });
}

/// エラー表示用の操作名。
fn op_label(op: &Op) -> String {
    match op {
        Op::CreatePlaylist => "リストの作成".into(),
        Op::DeletePlaylist => "リストの削除".into(),
        Op::Delete { item_id } => format!("動画の削除（{item_id}）"),
        Op::Insert { video_id, position, .. } => format!("動画の追加（動画 {video_id} / 位置 {position}）"),
        Op::Move { video_id, position, .. } => format!("動画の位置変更（動画 {video_id} / 位置 {position}）"),
    }
}

async fn exec_op(client: &YouTube, op: &Op, remote: &Option<String>, title: &str, appended: &mut bool) -> Result<Option<String>, String> {
    const NOT_CREATED: &str = "リストがまだ YouTube に作成されていません";
    match op {
        Op::CreatePlaylist => client.create_playlist(title).await.map(Some),
        Op::DeletePlaylist => match remote {
            Some(rid) => match client.delete_playlist(rid).await {
                // すでに無い、または自分のリストではない（他人のリストを一覧へ追加していた）場合は、一覧から外すだけにする
                Err(e) if e.contains("404") || e.contains("403") => Ok(None),
                r => r.map(|_| None),
            },
            None => Ok(None),
        },
        Op::Delete { item_id } => client.delete_item(item_id).await.map(|_| None),
        Op::Insert { video_id, position, .. } => match remote {
            Some(rid) => match client.insert_item(rid, video_id, Some(*position)).await {
                // 位置を受け付けないリスト（並び順が「手動」でない等）は、末尾に追加する
                Err(e) if e.contains("400") => {
                    *appended = true;
                    client.insert_item(rid, video_id, None).await.map(Some)
                }
                r => r.map(Some),
            },
            None => Err(NOT_CREATED.into()),
        },
        Op::Move { item_id, video_id, position } => match remote {
            Some(rid) => client.move_item(rid, item_id, video_id, *position).await.map(|_| None),
            None => Err(NOT_CREATED.into()),
        },
    }
}

/// クォータの回復（太平洋時間の0時）まで待つ。ウィンドウは最小化する。中止されたら false。
async fn wait_for_reset(handle: &AppHandle, done: usize, total: usize) -> bool {
    {
        let app = handle.state::<Mutex<Inner>>();
        save_deferred(&*app.lock().await); // 待機中にアプリを閉じても、次回起動時に続きから行えるようにする
    }
    let now = quota::now_secs();
    let resume_at = now + quota::secs_until_reset(now);
    emit_progress(handle, "waiting", done, total, "今日の API 上限に達しました。回復まで待機します".into(), resume_at);
    if let Some(w) = handle.get_webview_window("main") {
        let _ = w.minimize();
    }
    let ctl = handle.state::<PushCtl>();
    while quota::remaining() < quota::OP_COST {
        if ctl.cancel.load(Ordering::SeqCst) {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
    emit_progress(handle, "running", done, total, String::new(), 0);
    true
}

/// mode: now（今すぐ全件）/ defer（今日の残り分だけ行い、残りは保存して次回起動時に）/ wait（上限に達したら回復まで待って最後まで）
async fn push_inner(handle: &AppHandle, ids: &Option<Vec<String>>, mode: &str, progress: &mut (usize, usize)) -> Result<&'static str, String> {
    let app = handle.state::<Mutex<Inner>>();
    let conn = handle.state::<ConnectLock>();
    let ctl = handle.state::<PushCtl>();
    let client = client(&app, &conn).await?;
    progress.1 = pending_lists(&*app.lock().await, ids).iter().map(|p| p.count).sum();
    let mut touched: Vec<String> = Vec::new();
    let mut recreated: Vec<String> = Vec::new();
    let mut synced: Vec<String> = Vec::new();
    let outcome = loop {
        if ctl.cancel.load(Ordering::SeqCst) {
            break Ok("cancelled");
        }
        let next = {
            let g = app.lock().await;
            pending_lists(&g, ids).into_iter().find_map(|pl| {
                let p = g.playlists.get(&pl.id)?;
                Some((pl.id, p.plan().into_iter().next()?, p.remote_id.clone(), p.title.clone()))
            })
        };
        let Some((pid, op, remote, title)) = next else { break Ok("done") };
        if mode != "now" && quota::remaining() < quota::OP_COST {
            if mode == "defer" {
                break Ok("deferred");
            }
            if !wait_for_reset(handle, progress.0, progress.1).await {
                break Ok("cancelled");
            }
            continue;
        }
        // 位置の変更の前に、YouTube 上の実際の並びを取得し直す（追加時に位置を指定できなかった場合や、並び順を「手動」に切り替えた場合に、
        // 手元の想定とずれているため）。リストごとに1回だけ
        if matches!(op, Op::Move { .. }) && remote.is_some() && !synced.contains(&pid) {
            synced.push(pid.clone());
            if let Ok(fresh) = client.fetch_playlist(remote.as_deref().unwrap_or_default(), &title).await {
                quota::add(((fresh.items.len() + 49) / 50).max(1) as u32);
                let server_ids: Vec<String> = fresh.items.iter().map(|i| i.playlist_item_id.clone()).collect();
                if let Some(p) = app.lock().await.playlists.get_mut(&pid) {
                    p.set_server(server_ids);
                }
                progress.1 = pending_lists(&*app.lock().await, ids).iter().map(|p| p.count).sum::<usize>() + progress.0;
                continue;
            }
        }
        let label = op_label(&op);
        let mut appended = false;
        let result = exec_op(&client, &op, &remote, &title, &mut appended).await;
        // 位置を指定できず末尾に追加した場合は、サーバー側の並びもそのとおりに記録する（以降の位置変更で整える）
        let op = match &op {
            Op::Insert { item_id, video_id, .. } if appended => Op::Insert { item_id: item_id.clone(), video_id: video_id.clone(), position: usize::MAX },
            _ => op,
        };
        match result {
            Ok(result_id) => {
                quota::add(quota::OP_COST);
                let mut g = app.lock().await;
                if op == Op::DeletePlaylist {
                    g.playlists.remove(&pid);
                    g.summaries.retain(|s| s.id != pid);
                } else if let Some(p) = g.playlists.get_mut(&pid) {
                    p.apply(&op, result_id);
                }
                if !touched.contains(&pid) {
                    touched.push(pid);
                }
                progress.0 += 1;
                emit_progress(handle, "running", progress.0, progress.1, String::new(), 0);
            }
            Err(e) if quota::is_quota_error(&e) && mode != "now" => quota::mark_exhausted(),
            // このアプリで作ったリストが YouTube に見つからない（削除された等）: 1回だけ作り直す
            Err(e) if e.contains("404") && matches!(op, Op::Insert { .. } | Op::Move { .. }) && pid.starts_with("local-") && !recreated.contains(&pid) => {
                recreated.push(pid.clone());
                if let Some(p) = app.lock().await.playlists.get_mut(&pid) {
                    p.forget_remote();
                }
                progress.1 = pending_lists(&*app.lock().await, ids).iter().map(|p| p.count).sum::<usize>() + progress.0;
            }
            // 他人の公開リストなど、YouTube 上で削除権限のないリストを一覧から削除した場合は、手元から破棄して完了扱いにする
            Err(e) if op == Op::DeletePlaylist && e.contains("403") => {
                let mut g = app.lock().await;
                g.playlists.remove(&pid);
                g.summaries.retain(|s| s.id != pid);
                progress.0 += 1;
                emit_progress(handle, "running", progress.0, progress.1, String::new(), 0);
            }
            Err(e) => {
                let hint = if e.contains("403") && !quota::is_quota_error(&e) {
                    "\n\n【原因】このプレイリスト（または動画）の編集権限がありません（他人が作成した公開リストなど）。\nYouTube 側を変更できないため、ツリーで対象リストを右クリックして「YouTubeから最新を取得」を押して手元の未反映の変更を破棄するか、「一覧から削除」してください。"
                } else if e.contains("400") && matches!(op, Op::Insert { .. } | Op::Move { .. }) {
                    "\nこのリストの並び順が「手動」になっていない可能性があります。YouTube でリストの並び順を「手動」にしてからやり直してください"
                } else {
                    ""
                };
                break Err(format!("「{title}」で{}に失敗しました: {e}{hint}", label));
            }
        }
    };
    save_deferred(&*app.lock().await);
    for id in &touched {
        notify(handle, id);
    }
    outcome
}

/// 手元の変更を YouTube へ反映する（リスト作成・動画の削除/追加・位置の変更。いずれも1件50ユニット）。
/// バックグラウンドで実行し、進捗は push-progress イベントで通知する。
/// 1操作ごとに結果をモデルへ反映するので、途中で止まっても状態は一貫し、続きから反映できる。通信中は共有状態をロックしない。
#[tauri::command]
async fn start_push(handle: AppHandle, ctl: State<'_, PushCtl>, playlist_ids: Option<Vec<String>>, mode: String) -> Result<(), String> {
    if !matches!(mode.as_str(), "now" | "defer" | "wait") {
        return Err("unknown mode".into());
    }
    if ctl.running.swap(true, Ordering::SeqCst) {
        return Err("すでに反映処理を実行中です".into());
    }
    ctl.cancel.store(false, Ordering::SeqCst);
    tauri::async_runtime::spawn(async move {
        let mut progress = (0usize, 0usize);
        let result = push_inner(&handle, &playlist_ids, &mode, &mut progress).await;
        let (done, total) = progress;
        handle.state::<PushCtl>().running.store(false, Ordering::SeqCst);
        if let Some(w) = handle.get_webview_window("main") {
            if w.is_minimized().unwrap_or(false) {
                let _ = w.unminimize();
                let _ = w.request_user_attention(Some(tauri::UserAttentionType::Informational));
            }
        }
        match result {
            Ok(state) => emit_progress(&handle, state, done, total, String::new(), 0),
            Err(e) => emit_progress(&handle, "error", done, total, e, 0),
        }
    });
    Ok(())
}

#[tauri::command]
fn push_cancel(ctl: State<'_, PushCtl>) {
    ctl.cancel.store(true, Ordering::SeqCst);
}

/// リストを削除する。YouTube にあるリストは、一覧から隠したうえで Push 時に YouTube からも削除する。
#[tauri::command]
async fn remove_playlist(app: App<'_>, handle: AppHandle, playlist_id: String) -> Result<Vec<PlaylistSummary>, String> {
    let list = {
        let mut g = app.lock().await;
        let remote = match g.playlists.get(&playlist_id) {
            Some(p) => p.remote_id.clone(),
            None => Some(playlist_id.clone()),
        };
        match remote {
            // YouTube にあるリスト: 一覧から隠し、Push で YouTube からも削除する
            Some(rid) => {
                let title = g.summaries.iter().find(|s| s.id == playlist_id).map(|s| s.title.clone()).unwrap_or_default();
                let p = g.playlists.entry(playlist_id.clone()).or_insert_with(|| Playlist::new(rid, title, Vec::new()));
                p.deleted = true;
            }
            // 手元だけの新規リスト: そのまま破棄
            None => {
                g.summaries.retain(|s| s.id != playlist_id);
                g.playlists.remove(&playlist_id);
            }
        }
        g.list_summaries()
    };
    schedule_save(&handle);
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
    let win = WebviewWindowBuilder::new(&handle, &label, WebviewUrl::App(format!("editor.html?id={safe}").into()))
        .title(format!("並べ替え - {title}"))
        .inner_size(680.0, 820.0)
        .use_https_scheme(true)
        .disable_drag_drop_handler() // 画面内のドラッグ＆ドロップ並べ替えを有効にする
        .visible(false) // 保存済みの位置・大きさを適用してから表示する
        .build()
        .map_err(|e| e.to_string())?;
    winstate::restore(&win);
    let _ = win.show();
    Ok(())
}

#[derive(Serialize)]
struct CredentialsStatus {
    configured: bool,
    embedded: bool,
    dir: String,
}

fn credentials_status_now() -> CredentialsStatus {
    CredentialsStatus {
        configured: youtube::credentials_configured(),
        embedded: youtube::using_embedded(),
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

/// JSON の中身（ファイル選択で読み込んだ文字列）から認証情報を取り込む。
#[tauri::command]
async fn import_credentials_json(app: App<'_>, text: String) -> Result<CredentialsStatus, String> {
    let (id, secret) = youtube::parse_client_json(&text)?;
    save_credentials(app, id, secret).await
}

/// ダウンロード フォルダの最新の client_secret*.json から認証情報を取り込む。
#[tauri::command]
async fn import_downloaded_credentials(app: App<'_>) -> Result<CredentialsStatus, String> {
    let text = youtube::read_downloaded_client_json()?;
    import_credentials_json(app, text).await
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
        // ウィンドウの位置・大きさを window.ini に保存し、次回起動時に復元する
        .on_window_event(|window, event| {
            winstate::on_event(window, event);
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { .. } = event {
                    let h = window.app_handle().clone();
                    tauri::async_runtime::block_on(save_now(&h)); // 終了前に、取得済みの内容を保存する
                }
            }
        })
        .setup(|app| {
            // 前回取得した内容を読み込み、毎回取得し直さずに表示する。続けて、別日に回した未反映の変更を重ねる
            let st = app.state::<Mutex<Inner>>();
            tauri::async_runtime::block_on(async {
                let mut g = st.lock().await;
                if let Some(d) = quota::load_cache() {
                    restore(&mut g, d);
                }
                if let Some(d) = quota::load_deferred() {
                    restore(&mut g, d);
                }
            });
            if let Some(w) = app.get_webview_window("main") {
                winstate::restore(&w);
                let _ = w.show(); // 復元が済むまで隠してあったウィンドウを表示する
            }
            Ok(())
        })
        .manage(Mutex::new(Inner::default()))
        .manage(ConnectLock::default())
        .manage(PushCtl::default())
        .manage(SaveCtl::default())
        .invoke_handler(tauri::generate_handler![
            list_my_playlists,
            add_playlist,
            add_playlists,
            fetch_channel_playlists,
            get_playlist,
            refresh_playlist,
            list_summaries,
            create_playlist,
            set_video_playlists,
            drag_begin,
            video_info,
            get_video_channel,
            import_playlists,
            move_summary,
            peek_public_playlist,
            copy_peeked,
            move_between,
            sort_title,
            sort_items,
            shuffle,
            move_item,
            remove_items,
            reset,
            pending_count,
            pending_summary,
            push_plan,
            start_push,
            push_cancel,
            open_editor,
            remove_playlist,
            credentials_status,
            save_credentials,
            import_credentials_json,
            import_downloaded_credentials,
            clear_auth,
            app_info,
            open_url
        ])
        .run(tauri::generate_context!())
        .expect("failed to run app");
}
