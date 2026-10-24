const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const $ = (id) => document.getElementById(id);
const playlistId = new URLSearchParams(location.search).get("id");

let state = { items: [] };
let dragFrom = null;

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
      invoke(cmd, { playlistId, ...args }),
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
  $("plTitle").textContent = s.title;
  render();
  const n = await invoke("pending_count", { playlistId });
  $("writeBtn").textContent = `YouTubeへ書き戻し (${n}件 / ${n * 50}ユニット)`;
}

async function moveBy(idx, delta) {
  const s = await call("move_item", { fromIndex: idx, toIndex: idx + delta });
  if (s) setState(s);
}

function render() {
  const ul = $("list");
  ul.innerHTML = "";
  state.items.forEach((it, idx) => {
    const li = document.createElement("li");
    li.draggable = false;
    if (it.current_position !== it.original_position) li.classList.add("changed");
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
    li.addEventListener("dragstart", (e) => { dragFrom = idx; e.dataTransfer.effectAllowed = "move"; e.dataTransfer.setData("text/plain", String(idx)); });
    li.addEventListener("dragover", (e) => { if (dragFrom === null) return; e.preventDefault(); li.classList.add("dragover"); });
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

document.querySelectorAll("[data-act]").forEach((b) => b.onclick = async () => {
  const a = b.dataset.act;
  const s = a === "asc" ? await call("sort_title", { descending: false })
          : a === "desc" ? await call("sort_title", { descending: true })
          : a === "shuffle" ? await call("shuffle")
          : await call("reset");
  if (s) setState(s);
});

$("writeBtn").onclick = async () => {
  const n = await invoke("pending_count", { playlistId });
  if (!n) { alert("変更はありません。"); return; }
  if (!confirm(`${n}件を更新します（約${n * 50}ユニット消費）。\nプレイリストが「手動並べ替え」設定であることを確認してください。続行しますか？`)) return;
  const d = await call("write_back");
  if (d) { setState(d.state); status(`${d.updated}件を更新しました`); }
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
