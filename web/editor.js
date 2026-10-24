const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const $ = (id) => document.getElementById(id);
const playlistId = new URLSearchParams(location.search).get("id");

let state = { items: [] };
let dragFrom = null;

function status(msg) { $("status").textContent = msg; }

async function call(cmd, args) {
  status("処理中...");
  try {
    const data = await invoke(cmd, { playlistId, ...args });
    status("");
    return data;
  } catch (e) {
    status(String(e)); alert(e);
    return null;
  }
}

async function setState(s) {
  state = s;
  $("plTitle").textContent = s.title;
  render();
  const n = await invoke("pending_count", { playlistId });
  $("writeBtn").textContent = `YouTubeへ書き戻し (${n}件 / ${n * 50}ユニット)`;
}

function render() {
  const ul = $("list");
  ul.innerHTML = "";
  state.items.forEach((it, idx) => {
    const li = document.createElement("li");
    li.draggable = true;
    if (it.current_position !== it.original_position) li.classList.add("changed");
    li.innerHTML = `<span class="no">${idx + 1}</span><img loading="lazy"><span class="title"></span>`;
    li.querySelector("img").src = it.thumbnail_url;
    li.querySelector(".title").textContent = it.title;
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
