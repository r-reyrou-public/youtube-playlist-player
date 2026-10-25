const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const $ = (id) => document.getElementById(id);
const playlistId = new URLSearchParams(location.search).get("id");

let state = { items: [] };
let dragFrom = null;
const selected = new Set();   // 選択中の動画（playlist_item_id）。クリックで選択、Ctrl+クリックで複数選択

function status(msg) { $("status").textContent = msg; }

const CALL_LIMIT_MS = 200000;   // バックエンドの上限（認可待ち180秒）を超えたら画面側で打ち切る
let busy = 0;

function setBusy(on) {
  busy += on ? 1 : -1;
  document.querySelectorAll("header button[id], header [data-act]").forEach((b) => { if (!b.closest("#menu") && b.id !== "menuBtn") b.disabled = busy > 0; });
}

async function call(cmd, args, raw = false) {
  const t0 = Date.now();
  const tick = () => status(`処理中... ${Math.floor((Date.now() - t0) / 1000)}秒`);
  tick();
  const timer = setInterval(tick, 1000);
  setBusy(true);
  try {
    const data = await Promise.race([
      invoke(cmd, raw ? args : { playlistId, ...args }),
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

async function setState(s) {
  state = s;
  for (const id of [...selected]) if (!s.items.some((it) => it.playlist_item_id === id)) selected.delete(id);   // 外した・移した動画は選択から除く
  $("plTitle").textContent = s.title;
  render();
  const n = await invoke("pending_count", { playlistId });
  $("writeBtn").textContent = `YouTubeへ反映 (${n}件 / ${n * 50}ユニット)`;
}

// 選択中の動画（なければ target）をリストから取り除く。YouTube への反映は Push のとき
async function removeSelected(target) {
  const ids = selected.size ? [...selected] : target ? [target] : [];
  if (!ids.length) return;
  selected.clear();
  const s = await call("remove_items", { itemIds: ids });
  if (s) { setState(s); status(`${ids.length}本をリストから外しました（YouTube への反映は Push で。「元に戻す」で復活できます）`); }
}

async function moveBy(idx, delta) {
  const s = await call("move_item", { fromIndex: idx, toIndex: idx + delta });
  if (s) setState(s);
}

const CROSS_TYPE = "application/x-ytpl-item";

// この画面内のドラッグ、または別の編集ウィンドウからのドラッグを受け付ける
function acceptsDrag(e) {
  return dragFrom !== null || (e.dataTransfer && Array.from(e.dataTransfer.types).includes(CROSS_TYPE));
}

// index の位置へドロップ。同じ画面内なら並べ替え、別ウィンドウからなら移動（元のリストからは取り除く）
async function dropAt(e, index) {
  e.preventDefault();
  document.querySelectorAll(".dragover").forEach((el) => el.classList.remove("dragover"));
  if (dragFrom !== null) {
    const from = dragFrom;
    dragFrom = null;
    const to = Math.min(index, state.items.length - 1);
    if (from === to) return;
    const s = await call("move_item", { fromIndex: from, toIndex: to });
    if (s) setState(s);
  } else if (Array.from(e.dataTransfer.types).includes(CROSS_TYPE)) {
    const s = await call("move_between", { toPlaylist: playlistId, toIndex: index }, true);
    if (s) setState(s);
  }
}

// 一覧の余白（末尾）へのドロップ
const listArea = $("editorMain");
listArea.addEventListener("dragover", (e) => { if (acceptsDrag(e)) { e.preventDefault(); e.dataTransfer.dropEffect = "move"; } });
listArea.addEventListener("drop", (e) => dropAt(e, state.items.length));

function syncSelAll() {
  const n = state.items.filter((it) => selected.has(it.playlist_item_id)).length;
  $("selAll").checked = n > 0 && n === state.items.length;
  $("selAll").indeterminate = n > 0 && n < state.items.length;
  $("selCount").textContent = n ? `${n}本を選択中（Del キーでリストから外す）` : "";
}

$("selAll").onchange = () => {
  selected.clear();
  if ($("selAll").checked) state.items.forEach((it) => selected.add(it.playlist_item_id));
  render();
};

function render() {
  const ul = $("list");
  ul.innerHTML = "";
  syncSelAll();
  state.items.forEach((it, idx) => {
    const li = document.createElement("li");
    li.draggable = false;
    if (it.current_position !== it.original_position) li.classList.add("changed");
    if (it.is_new) li.classList.add("isnew");
    li.innerHTML = `<span class="handle" title="ドラッグして並べ替え">⠿</span><span class="no">${idx + 1}</span><img loading="lazy"><span class="title"></span>` +
      `<span class="mv"><button class="up" title="1つ上へ">▲</button><button class="down" title="1つ下へ">▼</button></span>`;
    const handle = li.querySelector(".handle");
    handle.addEventListener("mousedown", () => {
      li.draggable = true;
      // ハンドル外で離しても、行全体がドラッグ可能なまま残らないようにする
      document.addEventListener("mouseup", () => { li.draggable = false; }, { once: true });
    });
    li.addEventListener("dragend", () => {
      dragFrom = null;
      li.draggable = false;
      document.querySelectorAll(".dragover").forEach((el) => el.classList.remove("dragover"));
    });
    li.querySelector(".up").disabled = idx === 0;
    li.querySelector(".down").disabled = idx === state.items.length - 1;
    li.querySelector(".up").onclick = () => moveBy(idx, -1);
    li.querySelector(".down").onclick = () => moveBy(idx, 1);
    li.querySelector("img").src = it.thumbnail_url;
    li.querySelector(".title").textContent = it.title;
    if (selected.has(it.playlist_item_id)) li.classList.add("sel");
    li.addEventListener("click", (e) => {
      if (e.target.closest("button")) return;
      if (!e.ctrlKey && !e.metaKey) selected.clear();
      if (e.ctrlKey || e.metaKey) { selected.has(it.playlist_item_id) ? selected.delete(it.playlist_item_id) : selected.add(it.playlist_item_id); }
      else selected.add(it.playlist_item_id);
      render();
    });
    li.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      if (!selected.has(it.playlist_item_id)) { selected.clear(); selected.add(it.playlist_item_id); render(); }
      const n = selected.size;
      Common.showMenu(e.clientX, e.clientY, [
        { label: "リストに保存…", fn: () => Common.openPicker(it) },
        { label: "このチャンネルの再生リストを取得…", fn: () => Common.openChannelForVideo(it) },
        { label: n > 1 ? `選んだ ${n}本をリストから外す` : "リストから外す", danger: true, fn: () => removeSelected(it.playlist_item_id) },
      ]);
    });
    li.addEventListener("dragstart", (e) => {
      dragFrom = idx;
      e.dataTransfer.effectAllowed = "move";
      e.dataTransfer.setData("text/plain", it.title);
      e.dataTransfer.setData(CROSS_TYPE, "1");   // 他の編集ウィンドウが「この画面の動画」と判別する目印
      // 選択中の動画をドラッグした場合は、選択中の動画をまとめて移動する
      const ids = selected.has(it.playlist_item_id) && selected.size > 1
        ? state.items.filter((x) => selected.has(x.playlist_item_id)).map((x) => x.playlist_item_id)
        : [it.playlist_item_id];
      invoke("drag_begin", { playlistId, itemIds: ids }).catch(() => {});
    });
    li.addEventListener("dragover", (e) => {
      if (!acceptsDrag(e)) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";
      li.classList.add("dragover");
    });
    li.addEventListener("dragleave", () => li.classList.remove("dragover"));
    li.addEventListener("drop", (e) => { e.stopPropagation(); dropAt(e, idx); });
    ul.appendChild(li);
  });
}

document.addEventListener("keydown", (e) => {
  if (e.key !== "Delete" || /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement.tagName) || document.querySelector("dialog[open]")) return;
  removeSelected(null);
});

document.querySelectorAll("[data-act]").forEach((b) => b.onclick = async () => {
  const a = b.dataset.act;
  const s = a === "asc" ? await call("sort_title", { descending: false })
          : a === "desc" ? await call("sort_title", { descending: true })
          : a === "shuffle" ? await call("shuffle")
          : await call("reset");
  if (s) setState(s);
});

$("findBtn").onclick = () => Common.openFinder(playlistId, (it, idx) => {
  const li = $("list").children[idx];
  if (!li) return;
  li.scrollIntoView({ block: "center" });
  li.classList.add("found");
  setTimeout(() => li.classList.remove("found"), 2500);
});

$("writeBtn").onclick = async () => {
  try { await Common.openPush([playlistId]); } catch (e) { alert(e); }
  try { setState(await invoke("get_playlist", { playlistId })); } catch (_) {}
};

(async () => {
  const s = await call("get_playlist");
  if (s) setState(s);
})();

// 書き戻しが途中で失敗した場合などに、最新の状態へ追従する
listen("playlist-changed", async (e) => {
  if (e.payload !== playlistId) return;
  try { setState(await invoke("get_playlist", { playlistId })); } catch (_) {}
});
