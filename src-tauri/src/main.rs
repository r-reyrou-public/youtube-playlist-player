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
    /// ウィンドウ間ドラッグの移動元（playlist_id, playlist_item_id）。
    drag: Option<(String, String)>,
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

    /// 一覧用の要約。読み込み済みのリストは、編集中の件数・タイトル・新規かどうかを反映する。
    fn list_summaries(&self) -> Vec<PlaylistSummary> {
        self.summaries
            .iter()
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
    Ok(g.list_summaries())
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
    Ok(g.list_summaries())
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

/// 動画1本の情報（タイトル・サムネイル）を oEmbed で取得する。API のクォータは消費せず、ログインも不要。
#[tauri::command]
async fn video_info(video_id: String) -> Result<VideoInfo, String> {
    youtube::fetch_oembed(&video_id).await
}

/// ウィンドウ間ドラッグの開始: 移動元を覚えておく（ドロップ側のウィンドウが move_between で使う）。
#[tauri::command]
async fn drag_begin(app: App<'_>, playlist_id: String, item_id: String) -> Result<(), String> {
    app.lock().await.drag = Some((playlist_id, item_id));
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
        let (from, item_id) = g.drag.take().ok_or("移動元の情報がありません。もう一度ドラッグしてください")?;
        if from == to_playlist {
            return Err("同じリスト内の移動は、ウィンドウ内でドラッグしてください".into());
        }
        if !g.playlists.contains_key(&from) || !g.playlists.contains_key(&to_playlist) {
            return Err("プレイリストが読み込まれていません".into());
        }
        let item = g
            .playlists
            .get_mut(&from)
            .and_then(|p| p.take_item(&item_id))
            .ok_or("移動する動画が見つかりません")?;
        let new_id = g.next_item_id();
        let target = g.playlists.get_mut(&to_playlist).ok_or("プレイリストが読み込まれていません")?;
        target.add_video(
            VideoInfo { video_id: item.video_id, title: item.title, thumbnail_url: item.thumbnail_url, channel: item.channel },
            new_id,
            Some(to_index),
        );
        (from, target.clone())
    };
    notify(&handle, &from);
    notify(&handle, &to_playlist);
    Ok(out)
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
}

fn pending_lists(g: &Inner, ids: &Option<Vec<String>>) -> Vec<PendingPlaylist> {
    let mut out: Vec<PendingPlaylist> = g
        .playlists
        .values()
        .filter(|p| ids.as_ref().is_none_or(|v| v.contains(&p.playlist_id)))
        .filter_map(|p| {
            let count = p.plan().len();
            (count > 0).then(|| PendingPlaylist { id: p.playlist_id.clone(), title: p.title.clone(), count })
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

async fn exec_op(client: &YouTube, op: &Op, remote: &Option<String>, title: &str) -> Result<Option<String>, String> {
    const NOT_CREATED: &str = "リストがまだ YouTube に作成されていません";
    match op {
        Op::CreatePlaylist => client.create_playlist(title).await.map(Some),
        Op::Delete { item_id } => client.delete_item(item_id).await.map(|_| None),
        Op::Insert { video_id, position, .. } => match remote {
            Some(rid) => client.insert_item(rid, video_id, *position).await.map(Some),
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
        match exec_op(&client, &op, &remote, &title).await {
            Ok(result_id) => {
                quota::add(quota::OP_COST);
                let mut g = app.lock().await;
                if let Some(p) = g.playlists.get_mut(&pid) {
                    p.apply(&op, result_id);
                }
                if !touched.contains(&pid) {
                    touched.push(pid);
                }
                progress.0 += 1;
                emit_progress(handle, "running", progress.0, progress.1, String::new(), 0);
            }
            Err(e) if quota::is_quota_error(&e) && mode != "now" => quota::mark_exhausted(),
            Err(e) => break Err(e),
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

/// 一覧からプレイリストを外す（このアプリ内の管理のみ。YouTube 上のプレイリストは削除しない）。
#[tauri::command]
async fn remove_playlist(app: App<'_>, handle: AppHandle, playlist_id: String) -> Result<Vec<PlaylistSummary>, String> {
    let list = {
        let mut g = app.lock().await;
        g.summaries.retain(|s| s.id != playlist_id);
        g.playlists.remove(&playlist_id);
        g.list_summaries()
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
        .on_window_event(|window, event| winstate::on_event(window, event))
        .setup(|app| {
            // 前回、別日に回した未反映の変更があれば読み込む（画面側が Push の案内を出す）
            if let Some(d) = quota::load_deferred() {
                let st = app.state::<Mutex<Inner>>();
                tauri::async_runtime::block_on(async {
                    let mut g = st.lock().await;
                    g.next_local = g.next_local.max(d.next_local);
                    g.next_item = g.next_item.max(d.next_item);
                    for s in d.summaries {
                        g.upsert_summary(s);
                    }
                    for p in d.playlists {
                        g.playlists.insert(p.playlist_id.clone(), p);
                    }
                });
            }
            if let Some(w) = app.get_webview_window("main") {
                winstate::restore(&w);
                let _ = w.show(); // 復元が済むまで隠してあったウィンドウを表示する
            }
            Ok(())
        })
        .manage(Mutex::new(Inner::default()))
        .manage(ConnectLock::default())
        .manage(PushCtl::default())
        .invoke_handler(tauri::generate_handler![
            list_my_playlists,
            add_playlist,
            get_playlist,
            list_summaries,
            create_playlist,
            set_video_playlists,
            drag_begin,
            video_info,
            import_playlists,
            move_between,
            sort_title,
            sort_items,
            shuffle,
            move_item,
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
