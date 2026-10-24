let state = { playlist_id: null, items: [] };
let player = null, playerReady = false;
let nowVideoId = null;      // 再生中の video_id（並べ替えても追従）
let dragFrom = null;

const $ = (id) => document.getElementById(id);
const invoke = window.__TAURI__.core.invoke;

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

function setState(s) { state = s; render(); refreshPending(); }

async function refreshPending() {
  const n = await invoke("pending_count");
  $("writeBtn").textContent = `YouTubeへ書き戻し (${n}件 / ${n * 50}ユニット)`;
}

function render() {
  const ul = $("list");
  ul.innerHTML = "";
  state.items.forEach((it, idx) => {
    const li = document.createElement("li");
    li.draggable = true;
    if (it.video_id === nowVideoId) li.classList.add("playing");
    if (it.current_position !== it.original_position) li.classList.add("changed");
    li.innerHTML = `<span class="no">${idx + 1}</span><img loading="lazy"><span class="title"></span>`;
    li.querySelector("img").src = it.thumbnail_url;
    li.querySelector(".title").textContent = it.title;
    li.addEventListener("dblclick", () => playVideo(it.video_id));
    li.addEventListener("dragstart", () => { dragFrom = idx; });
    li.addEventListener("dragover", (e) => { e.preventDefault(); li.classList.add("dragover"); });
    li.addEventListener("dragleave", () => li.classList.remove("dragover"));
    li.addEventListener("drop", async (e) => {
      e.preventDefault();
      if (dragFrom === null || dragFrom === idx) return;
      const s = await call("move_item", { fromIndex: dragFrom, toIndex: idx });
      dragFrom = null;
      if (s) setState(s);
    });
    ul.appendChild(li);
  });
}

// ---- ツールバー ----
$("loadBtn").onclick = async () => {
  const s = await call("load", { playlistInput: $("plInput").value });
  if (s) { setState(s); status(`${s.items.length}件を取得`); }
};
document.querySelectorAll("[data-act]").forEach((b) => b.onclick = async () => {
  const a = b.dataset.act;
  const s = a === "asc" ? await call("sort_title", { descending: false })
          : a === "desc" ? await call("sort_title", { descending: true })
          : a === "shuffle" ? await call("shuffle")
          : await call("reset");
  if (s) setState(s);
});
$("writeBtn").onclick = async () => {
  const n = await invoke("pending_count");
  if (!n) { alert("変更はありません。"); return; }
  if (!confirm(`${n}件を更新します（約${n * 50}ユニット消費）。\nプレイリストが「手動並べ替え」設定であることを確認してください。続行しますか？`)) return;
  const d = await call("write_back");
  if (d) { setState(d.state); status(`${d.updated}件を更新しました`); }
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

function currentIndex() { return state.items.findIndex((i) => i.video_id === nowVideoId); }

function playVideo(videoId) {
  if (!playerReady) return;
  nowVideoId = videoId;
  player.loadVideoById(videoId);   // 並べ替え後の順序で自前キュー再生（IFrame APIのみ使用）
  const it = state.items[currentIndex()];
  $("musicTitle").textContent = it ? it.title : "";
  $("musicThumb").src = it ? it.thumbnail_url : "";
  render();
}

function playRelative(d) {
  if (!state.items.length) return;
  const i = currentIndex();
  if (!settings.loop && i + d >= state.items.length) return;   // ループ無効なら末尾で停止
  const n = state.items[(i + d + state.items.length) % state.items.length];
  playVideo(n.video_id);
}

$("prevBtn").onclick = () => playRelative(-1);
$("nextBtn").onclick = () => playRelative(1);
$("playBtn").onclick = () => {
  if (!playerReady) return;
  if (nowVideoId === null) { if (state.items.length) playVideo(state.items[0].video_id); return; }
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
