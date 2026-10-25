const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const $ = (id) => document.getElementById(id);

let summaries = [];          // [{id, title, item_count}]
const cache = {};            // playlist_id -> Playlist（展開済みのみ）
const expanded = new Set();
let player = null, playerReady = false;
let playOnce = false;   // 「この1本だけ再生」: 終わっても次へ進まない
let nowSingle = null;   // URL から再生中の単発の動画 { video_id, title, thumbnail_url }（リストには未登録）
let nowPlaylistId = null, nowVideoId = null;   // 再生中（並べ替えても video_id で追従）

function status(msg) { $("status").textContent = msg; $("status").title = msg; }

const CALL_LIMIT_MS = 200000;   // バックエンドの上限（認可待ち180秒）を超えたら画面側で打ち切る
let busy = 0;

function setBusy(on) {
  busy += on ? 1 : -1;
  ["addBtn", "myBtn", "pushBtn"].forEach((id) => { $(id).disabled = busy > 0; });
}

async function call(cmd, args) {
  const t0 = Date.now();
  const tick = () => status(`処理中... ${Math.floor((Date.now() - t0) / 1000)}秒`);
  tick();
  const timer = setInterval(tick, 1000);
  setBusy(true);
  try {
    const data = await Promise.race([
      invoke(cmd, args),
      new Promise((_, rej) => setTimeout(() => rej("応答がありません（タイムアウト）。もう一度お試しください。"), CALL_LIMIT_MS)),
    ]);
    status("");
    return data;
  } catch (e) {
    status(String(e)); alert(e);
    return null;
  } finally {
    clearInterval(timer);
    setBusy(false);
  }
}

// ---- 左ペイン: プレイリストのツリー ----
function renderTree() {
  const ul = $("tree");
  ul.innerHTML = "";
  for (const s of summaries) {
    const li = document.createElement("li");
    const head = document.createElement("div");
    head.className = "plname";
    head.title = "クリックで展開 / ダブルクリックで並べ替えウィンドウ";
    head.innerHTML = '<span class="arrow"></span><span class="name"></span><span class="badge" hidden>新規</span><span class="cnt"></span>';
    head.querySelector(".arrow").textContent = expanded.has(s.id) ? "▼" : "▶";
    head.querySelector(".name").textContent = s.title;
    head.querySelector(".badge").hidden = !s.is_new;
    head.querySelector(".cnt").textContent = s.item_count;
    // リスト名を別のリスト名へドラッグすると、その動画を丸ごと取り込む（コピー）
    head.draggable = true;
    head.addEventListener("dragstart", (e) => {
      dragList = s.id;
      e.dataTransfer.effectAllowed = "copy";
      e.dataTransfer.setData("text/plain", s.title);
    });
    head.addEventListener("dragend", () => {
      dragList = null;
      document.querySelectorAll(".dropTarget").forEach((el) => el.classList.remove("dropTarget"));
    });
    head.addEventListener("dragover", (e) => {
      if (!dragList || dragList === s.id) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "copy";
      head.classList.add("dropTarget");
    });
    head.addEventListener("dragleave", () => head.classList.remove("dropTarget"));
    head.addEventListener("drop", async (e) => {
      e.preventDefault();
      head.classList.remove("dropTarget");
      const from = summaries.find((x) => x.id === dragList);
      dragList = null;
      if (!from || from.id === s.id) return;
      if (!confirm(`「${from.title}」の動画（${from.item_count}本）を、「${s.title}」の末尾に取り込みます。\n元のリストは変わりません。YouTube への反映は Push で行います。\n続行しますか？`)) return;
      await doImport(s, [from.id]);
    });
    head.addEventListener("click", () => toggle(s.id));
    head.addEventListener("contextmenu", (e) => { e.preventDefault(); showCtx(e.clientX, e.clientY, s); });
    head.addEventListener("dblclick", () => call("open_editor", { playlistId: s.id, title: s.title }));
    li.appendChild(head);
    if (expanded.has(s.id)) li.appendChild(renderChildren(s.id));
    ul.appendChild(li);
  }
}

let drag = null;       // ツリー上の動画のドラッグ元 { id, idx }
let dragList = null;   // ツリー上のリスト名のドラッグ元（リスト id）

const importMsg = (target, r) =>
  `${r.added}本を「${target.title}」に取り込みました` + (r.skipped ? `（すでに入っている ${r.skipped}本はスキップ）` : "") + "。Push で YouTube に反映されます";

// sourceIds のリストの動画を、target のリストの末尾へ取り込む（ドラッグ操作用）
async function doImport(target, sourceIds) {
  for (const id of sourceIds) {
    if (!(await call("get_playlist", { playlistId: id }))) return;   // 未取得なら取得（取得済みなら通信しない）
  }
  const r = await call("import_playlists", { target: target.id, sources: sourceIds });
  if (r) status(importMsg(target, r));
}

function renderChildren(id) {
  const box = document.createElement("div");
  box.className = "children";
  const pl = cache[id];
  if (!pl) { box.innerHTML = '<div class="msg">読み込み中...</div>'; return box; }
  const ul = document.createElement("ul");
  ul.className = "vlist";
  pl.items.forEach((it, idx) => {
    const li = document.createElement("li");
    if (id === nowPlaylistId && it.video_id === nowVideoId) li.classList.add("playing");
    if (it.is_new) li.classList.add("isnew");
    li.innerHTML = `<span class="handle" title="ドラッグして並べ替え">⠿</span><span class="no">${idx + 1}</span><img loading="lazy"><span class="title"></span>`;
    li.querySelector("img").src = it.thumbnail_url;
    li.querySelector(".title").textContent = it.title;
    li.title = "ダブルクリックで再生 / 左のハンドルをドラッグで並べ替え";
    li.draggable = false;
    const handle = li.querySelector(".handle");
    handle.addEventListener("mousedown", () => {
      li.draggable = true;
      // ハンドル外で離しても、行全体がドラッグ可能なまま残らないようにする
      document.addEventListener("mouseup", () => { li.draggable = false; }, { once: true });
    });
    li.addEventListener("dragend", () => {
      drag = null;
      li.draggable = false;
      document.querySelectorAll(".dragover").forEach((el) => el.classList.remove("dragover"));
    });
    li.addEventListener("dblclick", () => playVideo(id, it.video_id));
    li.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      Common.showMenu(e.clientX, e.clientY, [
        { label: "ここから連続再生", fn: () => playVideo(id, it.video_id) },
        { label: "この1本だけ再生", fn: () => playVideo(id, it.video_id, 0, true) },
        { label: "リストに保存…", fn: () => Common.openPicker(it) },
      ]);
    });
    li.addEventListener("dragstart", (e) => { drag = { id, idx }; e.dataTransfer.effectAllowed = "move"; e.dataTransfer.setData("text/plain", String(idx)); });
    li.addEventListener("dragover", (e) => { if (drag && drag.id === id) { e.preventDefault(); li.classList.add("dragover"); } });
    li.addEventListener("dragleave", () => li.classList.remove("dragover"));
    li.addEventListener("drop", async (e) => {
      e.preventDefault();
      const from = drag;
      drag = null;
      if (!from || from.id !== id || from.idx === idx) { renderTree(); return; }
      const s = await call("move_item", { playlistId: id, fromIndex: from.idx, toIndex: idx });
      if (s) cache[id] = s;
      renderTree();
    });
    ul.appendChild(li);
  });
  box.appendChild(ul);
  return box;
}

// ---- 右クリックメニュー（一覧から削除） ----
let ctxTarget = null;
function showCtx(x, y, summary) {
  ctxTarget = summary;
  const m = $("ctxMenu");
  m.hidden = false;
  m.style.left = `${Math.min(x, window.innerWidth - m.offsetWidth - 4)}px`;
  m.style.top = `${Math.min(y, window.innerHeight - m.offsetHeight - 4)}px`;
}
const hideCtx = () => { $("ctxMenu").hidden = true; };
document.addEventListener("click", hideCtx);
document.addEventListener("keydown", (e) => { if (e.key === "Escape") hideCtx(); });
window.addEventListener("blur", hideCtx);
$("ctxEdit").onclick = () => {
  const s = ctxTarget;
  hideCtx();
  if (s) call("open_editor", { playlistId: s.id, title: s.title });
};
$("ctxFind").onclick = async () => {
  const s = ctxTarget;
  hideCtx();
  if (!s) return;
  Common.openFinder(s.id, async (it) => {
    expanded.add(s.id);
    if (!cache[s.id]) cache[s.id] = await call("get_playlist", { playlistId: s.id });
    renderTree();
    playVideo(s.id, it.video_id);
    document.querySelector(".vlist li.playing")?.scrollIntoView({ block: "center" });
  });
};
$("ctxImport").onclick = async () => {
  const s = ctxTarget;
  hideCtx();
  if (!s) return;
  const r = await Common.openImporter(s);
  if (r) status(importMsg(s, r));
};
$("ctxRemove").onclick = async () => {
  const s = ctxTarget;
  hideCtx();
  if (!s) return;
  const msg = s.is_new
    ? `「${s.title}」はまだ YouTube に作成されていない新規リストです。削除すると内容は破棄されます。`
    : `「${s.title}」をこのアプリの一覧から削除します。\nYouTube 上のプレイリストは削除されません（「マイリスト取得」で再び表示されます）。`;
  if (!confirm(msg)) return;
  const list = await call("remove_playlist", { playlistId: s.id });
  if (!list) return;
  summaries = list;
  delete cache[s.id];
  expanded.delete(s.id);
  renderTree();
};

async function toggle(id) {
  if (expanded.has(id)) { expanded.delete(id); renderTree(); return; }
  expanded.add(id);
  renderTree();
  if (!cache[id]) {
    const pl = await call("get_playlist", { playlistId: id });
    if (pl) cache[id] = pl; else expanded.delete(id);
    renderTree();
  }
}

// 並べ替えウィンドウでの変更・書き戻しを反映（キャッシュ再取得のみ。API通信なし）
listen("playlist-changed", async (e) => {
  const id = e.payload;
  try {
    if (cache[id]) cache[id] = await invoke("get_playlist", { playlistId: id });
    summaries = await invoke("list_summaries");
  } catch (_) { /* 削除済みのリストなど */ }
  renderTree();
  refreshPush();
});

// 左ペイン最上部: 新規リスト（手元に作る。YouTube への作成は Push）
$("newListBtn").onclick = async () => {
  const name = await Common.askText("新規リスト", "リスト名");
  if (!name) return;
  const list = await call("create_playlist", { title: name });
  if (list) { summaries = list; renderTree(); }
};

// ---- Push: 手元の変更を YouTube へ反映 ----
async function refreshPush() {
  const list = await invoke("pending_summary");
  const total = list.reduce((n, p) => n + p.count, 0);
  $("pushBtn").textContent = `Push (${total})`;
  return { list, total };
}

$("pushBtn").onclick = async () => {
  try { await Common.openPush(null); } catch (e) { alert(e); }
  await refreshPush();
};

// 前回、別日に回した未反映の変更があれば案内する（起動時）
async function offerDeferred() {
  const { total } = await refreshPush();
  if (!total) return;
  status(`前回の未反映の変更が${total}件あります`);
  if (confirm(`前回反映しきれなかった変更が${total}件あります（約${total * 50}ユニット）。\nPush の画面を開きますか？`)) $("pushBtn").click();
}

$("addBtn").onclick = async () => {
  const text = $("plInput").value.trim();
  if (!text) return;
  // list= を含む URL / ID はプレイリストとして追加。それ以外の動画の URL は再生のみ
  const isPlaylist = /[?&]list=/.test(text) || /^(PL|UU|FL|OL|LL)[A-Za-z0-9_-]{10,}$/.test(text);
  const v = isPlaylist ? null : Common.parseVideoInput(text);
  if (v) { await playSingle(v); return; }
  const list = await call("add_playlist", { playlistInput: text });
  if (list) { summaries = list; $("plInput").value = ""; renderTree(); }
};

// URL の動画を再生する（リストには登録しない）。同じ動画が再生中にもう一度押されたら、登録用のポップアップを出す。
async function playSingle(v) {
  if (nowSingle && nowSingle.video_id === v.videoId && nowVideoId === v.videoId) {
    Common.openPicker(nowSingle);   // 任意のリストに登録（複数選択・新規リスト作成）
    return;
  }
  if (!playerReady) { status("プレイヤーの準備ができていません"); return; }
  const info = await call("video_info", { videoId: v.videoId });   // oEmbed（クォータ消費なし）
  if (!info) return;
  nowSingle = { video_id: info.video_id, title: info.title, thumbnail_url: info.thumbnail_url };
  playVideo(null, v.videoId, v.start);
  status("再生中（リスト未登録）。リストに登録するには、もう一度「追加」を押してください");
}
$("myBtn").onclick = async () => {
  const list = await call("list_my_playlists");
  if (list) { summaries = list; renderTree(); status(`${list.length}件のプレイリスト`); }
};

// ---- プレイヤー ----
window.onYouTubeIframeAPIReady = () => {
  player = new YT.Player("player", {
    width: "100%", height: "100%",
    playerVars: { playsinline: 1, origin: location.origin },
    events: {
      onReady: () => { playerReady = true; },
      onStateChange: (e) => {
        if (e.data === YT.PlayerState.ENDED && !playOnce) playRelative(1);
        // 次の動画でも選択中の速度を維持する
        if (e.data === YT.PlayerState.PLAYING && player.getPlaybackRate() !== rate) player.setPlaybackRate(rate);
      },
      onError: () => { if (!playOnce) playRelative(1); },   // 再生不可動画はスキップ（1本だけ再生のときは進まない）
    },
  });
};

function queue() { return (cache[nowPlaylistId] || { items: [] }).items; }
function currentIndex() { return queue().findIndex((i) => i.video_id === nowVideoId); }

function currentItem() {
  return queue()[currentIndex()] || (nowSingle && nowSingle.video_id === nowVideoId ? nowSingle : null);
}

function playVideo(playlistId, videoId, startSeconds = 0, once = false) {
  if (!playerReady) return;
  playOnce = once;
  $("onceTag").hidden = !once;
  if (playlistId) nowSingle = null;   // リストの動画を再生したら、単発の動画の扱いは終わり
  nowPlaylistId = playlistId;
  nowVideoId = videoId;
  $("musicUrl").value = `https://www.youtube.com/watch?v=${videoId}`;
  player.loadVideoById({ videoId, startSeconds });   // 並べ替え後の順序で自前キュー再生（IFrame APIのみ使用）
  const it = currentItem();
  $("musicTitle").textContent = it ? it.title : "";
  $("musicThumb").src = it ? it.thumbnail_url : "";
  renderTree();
}

function playRelative(d) {
  const q = queue();
  if (!q.length) return;
  const i = currentIndex();
  if (!settings.loop && i + d >= q.length) return;   // ループ無効なら末尾で停止
  playVideo(nowPlaylistId, q[(i + d + q.length) % q.length].video_id);
}

$("prevBtn").onclick = () => playRelative(-1);
$("nextBtn").onclick = () => playRelative(1);
$("playBtn").onclick = () => {
  if (!playerReady) return;
  if (nowVideoId === null) { const q = Object.values(cache)[0]; if (q && q.items.length) playVideo(q.playlist_id, q.items[0].video_id); return; }
  player.getPlayerState() === YT.PlayerState.PLAYING ? player.pauseVideo() : player.playVideo();
};

// ---- 表示モード切替 ----
function setMode(music) {
  $("stage").className = music ? "music-mode" : "video-mode";
  $("modeMusic").classList.toggle("active", music);
  $("modeVideo").classList.toggle("active", !music);
}
$("modeVideo").onclick = () => setMode(false);
$("modeMusic").onclick = () => setMode(true);

// ---- シークバー ----
const fmt = (s) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
let seeking = false;
$("seek").onmousedown = () => { seeking = true; };
$("seek").onchange = () => {
  seeking = false;
  if (playerReady) player.seekTo(player.getDuration() * $("seek").value / 1000, true);
};
setInterval(() => {
  if (!playerReady || nowVideoId === null || seeking) return;
  const cur = player.getCurrentTime() || 0, dur = player.getDuration() || 0;
  $("seek").value = dur ? (cur / dur) * 1000 : 0;
  $("times").textContent = `${fmt(cur)} / ${fmt(dur)}`;
}, 500);

// ---- ハンバーガーメニュー / 設定 / ヘルプ ----
const settings = Object.assign({ mode: "video", loop: true },
  JSON.parse(localStorage.getItem("settings") || "{}"));
const saveSettings = () => localStorage.setItem("settings", JSON.stringify(settings));

setMode(settings.mode === "music");
$("setMode").value = settings.mode;
$("setLoop").checked = settings.loop;
$("setMode").onchange = () => { settings.mode = $("setMode").value; saveSettings(); setMode(settings.mode === "music"); };
$("setLoop").onchange = () => { settings.loop = $("setLoop").checked; saveSettings(); };

const menu = $("menu");
$("menuBtn").onclick = (e) => { e.stopPropagation(); menu.hidden = !menu.hidden; };
document.addEventListener("click", () => { menu.hidden = true; });
document.addEventListener("keydown", (e) => { if (e.key === "Escape") menu.hidden = true; });

async function refreshCred() {
  const st = await invoke("credentials_status");
  $("credState").textContent = st.embedded ? "（内蔵クライアント使用中・設定不要です）"
    : st.configured ? "（設定済み）" : "（未設定）";
  $("credState").className = st.configured ? "ok" : "ng";
  $("credDir").textContent = `保存先: ${st.dir}\\client_secret.json`;
}
async function afterImport(st) {
  if (!st) return;
  $("credId").value = "";
  $("credSecret").value = "";
  await refreshCred();
  alert("認証情報を保存しました。「マイリスト取得」でログインしてください。");
}
async function confirmReplace() {
  const st = await invoke("credentials_status");
  if (!st.configured) return true;
  return confirm("すでに認証情報が設定されています（内蔵クライアントを含む）。\n取り込むと、取り込んだ内容が優先され、保存済みのログイン情報（token.json）は削除されて再ログインが必要になります。続行しますか？");
}
$("credAuto").onclick = async () => {
  if (await confirmReplace()) afterImport(await call("import_downloaded_credentials"));
};
$("credFile").onclick = () => $("credFileInput").click();
$("credFileInput").onchange = async (e) => {
  const f = e.target.files[0];
  e.target.value = "";
  if (!f) return;
  if (await confirmReplace()) afterImport(await call("import_credentials_json", { text: await f.text() }));
};
$("credSave").onclick = async () => {
  const st = await call("save_credentials", { clientId: $("credId").value, clientSecret: $("credSecret").value });
  if (!st) return;
  $("credSecret").value = "";   // シークレットは画面に残さない
  await refreshCred();
  alert("保存しました。「マイリスト取得」でログインしてください。");
};
$("credClear").onclick = async () => {
  if (!confirm("保存済みのログイン情報を削除します。次回アクセス時に再度ブラウザで認可します。よろしいですか？")) return;
  if (await call("clear_auth") !== null) status("認可をリセットしました");
};
$("credConsole").onclick = () => invoke("open_url", { url: "https://console.cloud.google.com/apis/credentials" });
// 手順内のリンクは既定のブラウザで開く
document.querySelectorAll("a[data-url]").forEach((a) =>
  a.addEventListener("click", (e) => { e.preventDefault(); invoke("open_url", { url: a.dataset.url }); }));

$("openSettings").onclick = () => { refreshCred(); $("settingsDlg").showModal(); };
$("settingsClose").onclick = () => $("settingsDlg").close();
$("helpClose").onclick = () => $("helpDlg").close();
$("openHelp").onclick = async () => {
  const info = await invoke("app_info");
  $("helpVer").textContent = `v${info.version}`;
  const repo = $("helpRepo");
  repo.textContent = "";
  if (info.repository) {
    const a = document.createElement("a");
    a.textContent = info.repository;
    a.onclick = () => invoke("open_url", { url: info.repository });
    repo.appendChild(a);
  } else {
    repo.textContent = "未設定（Cargo.toml の repository に URL を設定）";
  }
  $("helpDlg").showModal();
};

// ---- 再生速度（メインパネルの下） ----
const SPEEDS = [0.25, 0.5, 0.75, 1, 1.25, 1.5, 1.75, 2];
let rate = 1;
$("speedSel").innerHTML = SPEEDS.map((v) => `<option value="${v}">${v}x</option>`).join("");
function applyRate() {
  $("speedSel").value = String(rate);
  if (playerReady) player.setPlaybackRate(rate);
}
$("speedSel").onchange = () => { rate = Number($("speedSel").value); applyRate(); };
$("speedReset").onclick = () => { rate = 1; applyRate(); };
applyRate();

// ---- 音楽モード右側のURL ----
$("urlCopy").onclick = async () => {
  const url = $("musicUrl").value;
  if (!url) return;
  try { await navigator.clipboard.writeText(url); status("URLをコピーしました"); }
  catch (e) { $("musicUrl").select(); document.execCommand("copy"); status("URLをコピーしました"); }
};
$("urlOpen").onclick = () => { const url = $("musicUrl").value; if (url) invoke("open_url", { url }); };

// ---- 画面下部のメモ（タイムスタンプ付き / 自由記述） ----
let memos = [];
try { memos = JSON.parse(localStorage.getItem("memos") || "[]"); } catch (_) { memos = []; }
const saveMemos = () => localStorage.setItem("memos", JSON.stringify(memos));

function fmtTime(sec) {
  const h = Math.floor(sec / 3600), m = Math.floor((sec % 3600) / 60), s = Math.floor(sec % 60);
  const ss = String(s).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}
const memoUrl = (m) => `https://www.youtube.com/watch?v=${m.videoId}&t=${m.time}s`;

// タイムスタンプのリンクを押したとき: 再生中の動画ならその位置へ、そうでなければその動画をその位置から再生
function jumpTo(m) {
  if (!playerReady) { status("プレイヤーの準備ができていません"); return; }
  if (m.videoId === nowVideoId) { player.seekTo(m.time, true); player.playVideo(); return; }
  playVideo(m.playlistId, m.videoId, m.time);
}

function renderMemos() {
  const ul = $("memoList");
  ul.textContent = "";
  if (!memos.length) {
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = "メモはまだありません。動画を再生して、下の欄で Enter を押すと再生位置が記録されます。";
    ul.appendChild(li);
    return;
  }
  for (const m of memos) {
    const li = document.createElement("li");
    if (m.time !== null && m.videoId) {
      const a = document.createElement("a");
      a.className = "ts";
      a.href = memoUrl(m);
      a.textContent = fmtTime(m.time);
      a.title = "クリックでこの位置から再生";
      a.addEventListener("click", (e) => { e.preventDefault(); jumpTo(m); });
      li.appendChild(a);
      const ext = document.createElement("button");
      ext.className = "ext";
      ext.textContent = "↗";
      ext.title = "ブラウザで YouTube を開く";
      ext.addEventListener("click", () => invoke("open_url", { url: memoUrl(m) }));
      li.appendChild(ext);
      if (m.title) {
        const vt = document.createElement("span");
        vt.className = "vt";
        vt.textContent = m.title;
        vt.title = m.title;
        li.appendChild(vt);
      }
    }
    const tx = document.createElement("span");
    tx.className = "tx";
    tx.textContent = m.text;
    tx.title = "ダブルクリックで編集";
    tx.addEventListener("dblclick", () => editMemo(m, tx));
    li.appendChild(tx);
    const del = document.createElement("button");
    del.className = "del";
    del.textContent = "×";
    del.title = "このメモを削除";
    del.addEventListener("click", () => { memos = memos.filter((x) => x.id !== m.id); saveMemos(); renderMemos(); });
    li.appendChild(del);
    ul.appendChild(li);
  }
  ul.scrollTop = ul.scrollHeight;
}

// 自由記述の部分をその場で編集（Enter で確定、Esc で取り消し）
function editMemo(m, span) {
  const input = document.createElement("input");
  input.className = "edit";
  input.value = m.text;
  span.replaceWith(input);
  input.focus();
  let done = false;
  const finish = (save) => {
    if (done) return;
    done = true;
    if (save) { m.text = input.value; saveMemos(); }
    renderMemos();
  };
  input.addEventListener("keydown", (e) => {
    if (e.isComposing || e.keyCode === 229) return;   // 日本語入力の変換確定の Enter は無視
    if (e.key === "Enter") finish(true);
    if (e.key === "Escape") finish(false);
  });
  input.addEventListener("blur", () => finish(true));
}

function addMemo() {
  const text = $("memoText").value.trim();
  const playing = playerReady && nowVideoId !== null;
  const stamp = $("memoStamp").checked && playing;
  if (!stamp && !text) { status(playing ? "メモを入力してください" : "再生中の動画がありません。メモを入力してください"); return; }
  const cur = currentItem();
  memos.push({
    id: `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    videoId: stamp ? nowVideoId : null,
    playlistId: stamp ? nowPlaylistId : null,
    title: stamp && cur ? cur.title : "",
    time: stamp ? Math.floor(player.getCurrentTime() || 0) : null,
    text,
    ts: Date.now(),
  });
  saveMemos();
  $("memoText").value = "";
  renderMemos();
}

$("memoAdd").onclick = addMemo;
$("memoText").addEventListener("keydown", (e) => {
  // 日本語入力の変換確定の Enter（isComposing / keyCode 229）では追加しない。Shift+Enter は改行
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing && e.keyCode !== 229) {
    e.preventDefault();
    addMemo();
  }
});

$("memoCopy").onclick = async () => {
  if (!memos.length) return;
  const lines = memos.map((m) => {
    const head = m.time !== null && m.videoId ? `[${fmtTime(m.time)}] ${memoUrl(m)}` : "";
    const title = m.title ? `「${m.title}」` : "";
    return [head, title, m.text].filter(Boolean).join(" ");
  });
  try { await navigator.clipboard.writeText(lines.join("\n")); status("メモをコピーしました"); }
  catch (_) { status("コピーできませんでした"); }
};
$("memoClear").onclick = () => {
  if (!memos.length) return;
  if (!confirm(`メモ ${memos.length} 件をすべて削除します。よろしいですか？`)) return;
  memos = [];
  saveMemos();
  renderMemos();
};
renderMemos();

// ---- 左ペイン（リスト）を畳む ----
function setTreeCollapsed(c) {
  $("treePane").classList.toggle("collapsed", c);
  $("treeToggle").textContent = c ? "»" : "«";
  localStorage.setItem("treeCollapsed", c ? "1" : "0");
}
$("treeToggle").onclick = () => setTreeCollapsed(!$("treePane").classList.contains("collapsed"));
setTreeCollapsed(localStorage.getItem("treeCollapsed") === "1");

// 起動時: 前回保存した未反映の変更（別日に回したもの）を読み込んで案内する
(async () => {
  try { summaries = await invoke("list_summaries"); renderTree(); } catch (_) {}
  setTimeout(offerDeferred, 300);
})();
