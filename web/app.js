const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const $ = (id) => document.getElementById(id);

let summaries = [];          // [{id, title, item_count}]
const cache = {};            // playlist_id -> Playlist（展開済みのみ）
const expanded = new Set();
let player = null, playerReady = false;
let nowPlaylistId = null, nowVideoId = null;   // 再生中（並べ替えても video_id で追従）

function status(msg) { $("status").textContent = msg; }

async function call(cmd, args) {
  status("処理中...");
  try {
    const data = await invoke(cmd, args);
    status("");
    return data;
  } catch (e) {
    status(String(e)); alert(e);
    return null;
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
  if (!cache[id]) return;
  cache[id] = await invoke("get_playlist", { playlistId: id });
  renderTree();
});

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

$("openSettings").onclick = () => $("settingsDlg").showModal();
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
