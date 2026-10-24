const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const $ = (id) => document.getElementById(id);

let summaries = [];          // [{id, title, item_count}]
const cache = {};            // playlist_id -> Playlist（展開済みのみ）
const expanded = new Set();
let player = null, playerReady = false;
let nowPlaylistId = null, nowVideoId = null;   // 再生中（並べ替えても video_id で追従）

function status(msg) { $("status").textContent = msg; }

const CALL_LIMIT_MS = 200000;   // バックエンドの上限（認可待ち180秒）を超えたら画面側で打ち切る
let busy = 0;

function setBusy(on) {
  busy += on ? 1 : -1;
  document.querySelectorAll("header button[id], header [data-act]").forEach((b) => { if (!b.closest("#menu") && b.id !== "menuBtn") b.disabled = busy > 0; });
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
    head.innerHTML = '<span class="arrow"></span><span class="name"></span><span class="cnt"></span>';
    head.querySelector(".arrow").textContent = expanded.has(s.id) ? "▼" : "▶";
    head.querySelector(".name").textContent = s.title;
    head.querySelector(".cnt").textContent = s.item_count;
    head.addEventListener("click", () => toggle(s.id));
    head.addEventListener("contextmenu", (e) => { e.preventDefault(); showCtx(e.clientX, e.clientY, s); });
    head.addEventListener("dblclick", () => call("open_editor", { playlistId: s.id, title: s.title }));
    li.appendChild(head);
    if (expanded.has(s.id)) li.appendChild(renderChildren(s.id));
    ul.appendChild(li);
  }
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
    li.innerHTML = `<span class="no">${idx + 1}</span><img loading="lazy"><span class="title"></span>`;
    li.querySelector("img").src = it.thumbnail_url;
    li.querySelector(".title").textContent = it.title;
    li.title = "ダブルクリックで再生";
    li.addEventListener("dblclick", () => playVideo(id, it.video_id));
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
$("ctxRemove").onclick = async () => {
  const s = ctxTarget;
  hideCtx();
  if (!s) return;
  if (!confirm(`「${s.title}」をこのアプリの一覧から削除します。
YouTube 上のプレイリストは削除されません（「マイリスト取得」で再び表示されます）。`)) return;
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
  if (cache[id]) {
    cache[id] = await invoke("get_playlist", { playlistId: id });
    renderTree();
  }
  refreshPush();
});

// ---- Push: 手元の変更を YouTube へ反映 ----
async function refreshPush() {
  const list = await invoke("pending_summary");
  const total = list.reduce((n, p) => n + p.count, 0);
  $("pushBtn").textContent = `Push (${total})`;
  return { list, total };
}

$("pushBtn").onclick = async () => {
  const { list, total } = await refreshPush();
  if (!total) { alert("YouTube へ反映する変更はありません。"); return; }
  const lines = list.map((p) => `・${p.title}: ${p.count}件`).join("\n");
  const warn = total * 50 > 10000 ? "\n\n⚠ 1日の API 上限（10,000ユニット）を超えます。途中で失敗する可能性があります。" : "";
  if (!confirm(`次の変更を YouTube に反映します（約${total * 50}ユニット消費）。\n\n${lines}${warn}\n\n対象は「手動並べ替え」設定のプレイリストです。続行しますか？`)) return;
  let done = 0;
  for (const p of list) {
    const r = await call("write_back", { playlistId: p.id });
    if (!r) { await refreshPush(); return; }   // 失敗したらそこで中断（エラーは表示済み）
    done += r.updated;
    if (cache[p.id]) { cache[p.id] = r.state; renderTree(); }
  }
  await refreshPush();
  status(`${done}件をYouTubeへ反映しました`);
};

$("addBtn").onclick = async () => {
  const list = await call("add_playlist", { playlistInput: $("plInput").value });
  if (list) { summaries = list; $("plInput").value = ""; renderTree(); }
};
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
      onStateChange: (e) => { if (e.data === YT.PlayerState.ENDED) playRelative(1); },
      onError: () => playRelative(1),   // 再生不可動画はスキップ
    },
  });
};

function queue() { return (cache[nowPlaylistId] || { items: [] }).items; }
function currentIndex() { return queue().findIndex((i) => i.video_id === nowVideoId); }

function playVideo(playlistId, videoId) {
  if (!playerReady) return;
  nowPlaylistId = playlistId;
  nowVideoId = videoId;
  player.loadVideoById(videoId);   // 並べ替え後の順序で自前キュー再生（IFrame APIのみ使用）
  const it = queue()[currentIndex()];
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
  $("credState").textContent = st.configured ? "（設定済み）" : "（未設定）";
  $("credState").className = st.configured ? "ok" : "ng";
  $("credDir").textContent = `保存先: ${st.dir}\\client_secret.json`;
}
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
