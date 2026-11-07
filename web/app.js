const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const $ = (id) => document.getElementById(id);

let summaries = [];          // [{id, title, item_count}]
const cache = {};            // playlist_id -> Playlist（展開済みのみ）
const expanded = new Set();
const durationsFetched = new Set();
let player = null, playerReady = false;
let playOnce = false;   // 「この1本だけ再生」: 終わっても次へ進まない
let nowSingle = null;   // URL から再生中の単発の動画 { video_id, title, thumbnail_url }（リストには未登録）
let nowPlaylistId = null, nowVideoId = null;   // 再生中（並べ替えても video_id で追従）

function status(msg) { $("status").textContent = msg; $("status").title = msg; }

const CALL_LIMIT_MS = 200000;   // バックエンドの上限（認可待ち180秒）を超えたら画面側で打ち切る
let busy = 0;

function setBusy(on) {
  busy += on ? 1 : -1;
  ["addBtn", "copyBtn", "myBtn", "pushBtn"].forEach((id) => { $(id).disabled = busy > 0; });
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

// プレイリストを先頭から（またはシャッフルして）直接再生する
async function playPlaylist(id, shuffle = false) {
  if (!playerReady) { status("プレイヤーの準備ができていません"); return; }
  if (id === nowPlaylistId && !shuffle) {
    const st = player.getPlayerState();
    if (st === YT.PlayerState.PLAYING) player.pauseVideo();
    else player.playVideo();
    updatePlayIcons();
    return;
  }
  let pl = cache[id];
  if (!pl || !pl.items || !pl.items.length) {
    status("プレイリストを読み込み中...");
    pl = await call("get_playlist", { playlistId: id });
    if (pl) {
      cache[id] = pl;
      durationsFetched.add(id);
      summaries = await call("list_summaries");
      renderTree();
    }
  }
  if (!pl || !pl.items || !pl.items.length) {
    status("再生できる動画がありません");
    return;
  }
  expanded.add(id);
  if (shuffle) {
    if (settings.shuffle === "off") {
      settings.shuffle = "list";
      saveSettings();
      updateShuffleUi();
    }
  }
  let target = pl.items[0];
  if (shuffle) {
    const idx = Math.floor(Math.random() * pl.items.length);
    target = pl.items[idx];
  }
  playVideo(id, target.video_id);
  status(`「${pl.title}」を${shuffle ? "シャッフル" : ""}再生開始`);
}

// ---- 左ペイン: プレイリストのツリー ----

function updatePlayIcons() {
  if (!playerReady) return;
  const isPlaying = player.getPlayerState() === YT.PlayerState.PLAYING;
  document.querySelectorAll('#tree .plPlayBtn').forEach((btn) => {
    const li = btn.closest('li');
    const plPlaying = li && li.classList.contains('playing');
    btn.textContent = (plPlaying && isPlaying) ? '⏸' : '▶';
    btn.title = (plPlaying && isPlaying) ? '一時停止' : 'このリストを再生';
  });
  document.querySelectorAll('.vlist li').forEach((li) => {
    const vPlay = li.querySelector('.vPlayBtn');
    if (!vPlay) return;
    const vPlaying = li.classList.contains('playing');
    vPlay.textContent = (vPlaying && isPlaying) ? '⏸' : '▶';
    vPlay.title = (vPlaying && isPlaying) ? '一時停止' : 'この動画を再生';
  });
}

function renderTree() {
  const treePane = $("treePane");
  const prevScroll = treePane ? treePane.scrollTop : 0;
  const ul = $("tree");
  ul.innerHTML = "";
  for (const s of summaries) {
    const li = document.createElement("li");
    const isPlaying = s.id === nowPlaylistId;
    if (isPlaying) li.classList.add("playing");
    const head = document.createElement("div");
    head.className = "plname" + (isPlaying ? " playing" : "");
    const got = cache[s.id] && cache[s.id].fetched_at;
    const pl = cache[s.id];
    let totalSecs = s.total_duration_secs || 0;
    if (pl && pl.items) {
      totalSecs = pl.items.reduce((acc, it) => acc + (it.duration_secs || 0), 0);
    }
    const durStr = totalSecs > 0 ? Common.formatDuration(totalSecs) : (s.total_duration || "");
    const durTip = totalSecs > 0 ? `\n総再生時間: ${Common.formatDurationFriendly(totalSecs)}` : "";

    head.title = "クリックで展開 / ダブルクリックで並べ替えウィンドウ / ドラッグ: 上下の端にドロップで並び順の変更、中央にドロップで動画の取り込み" + (got ? `\n取得: ${new Date(got * 1000).toLocaleString("ja-JP")} 時点（右クリックで最新を取得）` : "") + durTip;
    head.innerHTML = '<span class="arrow"></span><button class="plPlayBtn" title="このリストを再生">▶</button><span class="name"></span><span class="badge" hidden>新規</span><span class="plMeta"><span class="cnt"></span><span class="plDuration" hidden></span></span>';
    head.querySelector(".arrow").textContent = expanded.has(s.id) ? "▼" : "▶";

    const playBtn = head.querySelector(".plPlayBtn");
    if (isPlaying) playBtn.classList.add("playing");
    playBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      playPlaylist(s.id);
    });
    playBtn.addEventListener("dblclick", (e) => {
      e.stopPropagation();
    });

    head.querySelector(".name").textContent = s.title;
    head.querySelector(".badge").hidden = !s.is_new;
    head.querySelector(".cnt").textContent = `${s.item_count}本`;
    const durEl = head.querySelector(".plDuration");
    if (durStr) {
      durEl.textContent = durStr;
      durEl.hidden = false;
      durEl.title = `総再生時間: ${Common.formatDurationFriendly(totalSecs) || durStr}`;
    } else {
      durEl.hidden = true;
    }
    // リスト名を別のリスト名へドラッグすると、その動画を丸ごと取り込む（コピー）
    head.draggable = true;
    head.addEventListener("dragstart", (e) => {
      if (e.target.closest(".plPlayBtn") || e.target.closest(".arrow")) {
        e.preventDefault();
        return;
      }
      dragList = s.id;
      e.dataTransfer.effectAllowed = "copyMove";
      e.dataTransfer.setData("text/plain", s.title);
    });
    head.addEventListener("dragend", () => {
      dragList = null;
      document.querySelectorAll(".dropTarget, .dropBefore, .dropAfter").forEach((el) => el.classList.remove("dropTarget", "dropBefore", "dropAfter"));
    });
    // 行の上端・下端にドロップすると並び順の変更、中央にドロップすると取り込み（コピー）
    const zone = (e) => {
      const r = head.getBoundingClientRect();
      const y = (e.clientY - r.top) / r.height;
      return y < 0.28 ? "before" : y > 0.72 ? "after" : "import";
    };
    const clearDrop = () => head.classList.remove("dropTarget", "dropBefore", "dropAfter");
    head.addEventListener("dragover", (e) => {
      if (!dragList || dragList === s.id) return;
      e.preventDefault();
      const z = zone(e);
      e.dataTransfer.dropEffect = z === "import" ? "copy" : "move";
      clearDrop();
      head.classList.add(z === "before" ? "dropBefore" : z === "after" ? "dropAfter" : "dropTarget");
    });
    head.addEventListener("dragleave", clearDrop);
    head.addEventListener("drop", async (e) => {
      e.preventDefault();
      const z = zone(e);
      clearDrop();
      const from = summaries.find((x) => x.id === dragList);
      dragList = null;
      if (!from || from.id === s.id) return;
      if (z !== "import") {
        await moveList(from.id, s.id, z);
        return;
      }
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
  if (treePane) treePane.scrollTop = prevScroll;
  updatePlayIcons();
}

// リストの並び順を変える（targetId の before または after へ）
async function moveList(fromId, targetId, position) {
  const list = await call("move_summary", { fromId, targetId, position });
  if (!list) return;
  summaries = list;
  renderTree();
}

const selected = { id: null, items: new Set() };   // ツリーで選択中の動画（リスト id, playlist_item_id）。Ctrl+クリックで複数選択

function syncSel(ul, id) {
  const items = cache[id] ? cache[id].items : [];
  [...ul.children].forEach((x, i) => x.classList.toggle("sel", selected.id === id && !!items[i] && selected.items.has(items[i].playlist_item_id)));
  // 「全選択」チェックボックスと選択数を同期
  const bar = ul.parentElement.querySelector(".selBar");
  if (!bar) return;
  const n = selected.id === id ? items.filter((x) => selected.items.has(x.playlist_item_id)).length : 0;
  const cb = bar.querySelector("input");
  cb.checked = n > 0 && n === items.length;
  cb.indeterminate = n > 0 && n < items.length;
  bar.querySelector(".selCount").textContent = n ? `${n}本を選択中（Del キーでリストから外す）` : "";
}

// 選択中の動画（なければ target）をリストから取り除く。YouTube への反映は Push のとき
async function removeSelected(id, target) {
  const ids = selected.id === id && selected.items.size ? [...selected.items] : target ? [target] : [];
  if (!ids.length) return;
  selected.id = null; selected.items.clear();
  const pl = await call("remove_items", { playlistId: id, itemIds: ids });
  if (!pl) return;
  cache[id] = pl;
  summaries = await invoke("list_summaries");
  renderTree();
  refreshPush();
  status(`${ids.length}本をリストから外しました（YouTube への反映は Push で。編集ウィンドウの「元に戻す」で復活できます）`);
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
  const bar = document.createElement("div");
  bar.className = "selBar";
  bar.innerHTML = '<label><input type="checkbox"> 全選択</label><span class="selCount"></span>';
  const all = bar.querySelector("input");
  all.onchange = () => {
    selected.id = id; selected.items.clear();
    if (all.checked) pl.items.forEach((x) => selected.items.add(x.playlist_item_id));
    else selected.id = null;
    syncSel(box.querySelector(".vlist"), id);
  };
  box.appendChild(bar);
  const ul = document.createElement("ul");
  ul.className = "vlist";
  pl.items.forEach((it, idx) => {
    const li = document.createElement("li");
    if (id === nowPlaylistId && it.video_id === nowVideoId) li.classList.add("playing");
    if (it.is_new) li.classList.add("isnew");
    li.innerHTML = `<span class="handle" title="ドラッグして並べ替え">⠿</span><button class="vPlayBtn" title="この動画を再生">▶</button><span class="no">${idx + 1}</span><img loading="lazy" title="クリックで再生"><span class="title"></span>${it.duration ? `<span class="duration">${it.duration}</span>` : ""}`;
    const img = li.querySelector("img");
    img.src = it.thumbnail_url;
    img.addEventListener("click", (e) => {
      e.stopPropagation();
      playVideo(id, it.video_id);
    });
    img.addEventListener("dblclick", (e) => e.stopPropagation());
    const vPlay = li.querySelector(".vPlayBtn");
    vPlay.addEventListener("click", (e) => {
      e.stopPropagation();
      if (id === nowPlaylistId && it.video_id === nowVideoId && playerReady) {
        const st = player.getPlayerState();
        if (st === YT.PlayerState.PLAYING) player.pauseVideo();
        else player.playVideo();
        updatePlayIcons();
      } else {
        playVideo(id, it.video_id);
      }
    });
    vPlay.addEventListener("dblclick", (e) => e.stopPropagation());
    li.querySelector(".title").textContent = it.title;
    li.title = "クリックで選択 / ダブルクリックまたは▶で再生 / 左のハンドルをドラッグで並べ替え";
    li.draggable = true;
    li.addEventListener("dragend", () => {
      drag = null;
      document.querySelectorAll(".dragover").forEach((el) => el.classList.remove("dragover"));
    });
    li.addEventListener("dblclick", () => playVideo(id, it.video_id));
    if (selected.id === id && selected.items.has(it.playlist_item_id)) li.classList.add("sel");
    li.addEventListener("click", (e) => {
      if (e.target.closest(".handle")) return;
      const multi = e.ctrlKey || e.metaKey;
      if (selected.id !== id || !multi) { selected.id = id; selected.items.clear(); }
      if (multi && selected.items.has(it.playlist_item_id)) selected.items.delete(it.playlist_item_id);
      else selected.items.add(it.playlist_item_id);
      syncSel(li.parentElement, id);   // 再描画しない（ダブルクリックでの再生を妨げないため）
    });
    li.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      if (selected.id !== id || !selected.items.has(it.playlist_item_id)) { selected.id = id; selected.items.clear(); selected.items.add(it.playlist_item_id); syncSel(li.parentElement, id); }
      const n = selected.items.size;
      Common.showMenu(e.clientX, e.clientY, [
        { label: "ここから連続再生", fn: () => playVideo(id, it.video_id) },
        { label: "この1本だけ再生", fn: () => playVideo(id, it.video_id, 0, true) },
        { label: "リストに保存…", fn: () => Common.openPicker(it) },
        { label: "このチャンネルの再生リストを取得…", fn: () => openChannelForVideo(it) },
        { label: n > 1 ? `選んだ ${n}本をリストから外す` : "リストから外す", danger: true, fn: () => removeSelected(id, it.playlist_item_id) },
      ]);
    });
    li.addEventListener("dragstart", (e) => {
      if (!e.target.closest(".handle")) {
        e.preventDefault();
        return;
      }
      drag = { id, idx };
      e.dataTransfer.effectAllowed = "move";
      e.dataTransfer.setData("text/plain", String(idx));
    });
    li.addEventListener("dragover", (e) => {
      if (drag && drag.id === id) {
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
        li.classList.add("dragover");
      }
    });
    li.addEventListener("dragleave", () => li.classList.remove("dragover"));
    li.addEventListener("drop", async (e) => {
      e.preventDefault();
      e.stopPropagation();
      const from = drag;
      drag = null;
      document.querySelectorAll(".dragover").forEach((el) => el.classList.remove("dragover"));
      if (!from || from.id !== id || from.idx === idx) { return; }
      const s = await call("move_item", { playlistId: id, fromIndex: from.idx, toIndex: idx });
      if (s) {
        cache[id] = s;
        renderTree();
        refreshPush();
      }
    });
    ul.appendChild(li);
  });
  box.appendChild(ul);
  syncSel(ul, id);
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
const shiftList = async (delta) => {
  const s = ctxTarget;
  hideCtx();
  if (!s) return;
  const i = summaries.findIndex((x) => x.id === s.id);
  const j = i + delta;
  if (i < 0 || j < 0 || j >= summaries.length) return;
  // 上へ: 一つ上の直前 / 下へ: 一つ下のさらに次の直前（末尾なら null）
  const before = delta < 0 ? summaries[j].id : (summaries[j + 1] ? summaries[j + 1].id : null);
  await moveList(s.id, before);
};
$("ctxPlay").onclick = async () => {
  const s = ctxTarget;
  hideCtx();
  if (s) await playPlaylist(s.id, false);
};
$("ctxShuffle").onclick = async () => {
  const s = ctxTarget;
  hideCtx();
  if (s) await playPlaylist(s.id, true);
};
$("ctxUp").onclick = () => shiftList(-1);
$("ctxDown").onclick = () => shiftList(1);
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
$("ctxRefresh").onclick = async () => {
  const s = ctxTarget;
  hideCtx();
  if (!s) return;
  if (s.is_new) { alert("まだ YouTube に作成されていない新規リストは、取得し直せません。"); return; }
  const n = await invoke("pending_count", { playlistId: s.id });
  if (n && !confirm(`「${s.title}」には YouTube に未反映の変更が${n}件あります。最新を取得すると、これらの変更は破棄されます。続行しますか？`)) return;
  const pl = await call("refresh_playlist", { playlistId: s.id });
  if (!pl) return;
  cache[s.id] = pl;
  summaries = await invoke("list_summaries");
  renderTree();
  refreshPush();
  status(`「${s.title}」を最新の内容に更新しました`);
};
$("ctxImport").onclick = async () => {
  const s = ctxTarget;
  hideCtx();
  if (!s) return;
  const r = await Common.openImporter(s);
  if (r) status(importMsg(s, r));
};
$("ctxCompare").onclick = async () => {
  const s = ctxTarget;
  hideCtx();
  if (!s) return;
  openDedupModal("compare", s.id);
};
$("ctxDedupModal").onclick = async () => {
  const s = ctxTarget;
  hideCtx();
  if (!s) return;
  openDedupModal("single", s.id);
};
$("ctxDedup").onclick = async () => {
  const s = ctxTarget;
  hideCtx();
  if (!s) return;
  const res = await call("dedup_playlist", { playlistId: s.id });
  if (res) {
    if (res.playlist) cache[s.id] = res.playlist;
    renderTree();
    refreshPush();
    alert(res.removed_count > 0
      ? `「${s.title}」から ${res.removed_count} 件の重複動画を削除しました。`
      : `「${s.title}」に重複している動画はありませんでした。`);
  }
};
$("ctxShorts").onclick = async () => {
  const s = ctxTarget;
  hideCtx();
  if (!s) return;
  const count = await call("remove_shorts", { playlistId: s.id });
  if (typeof count === "number") {
    cache[s.id] = await call("get_playlist", { playlistId: s.id });
    summaries = await call("list_summaries");
    renderTree();
    refreshPush();
    alert(count > 0
      ? `「${s.title}」から ${count} 本のショート動画（60秒以下）を削除しました。`
      : `「${s.title}」にショート動画はありませんでした。`);
  }
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
  // リストを開いた際に再生時間を取得し、ショート動画を削除
  const needFetch = !cache[id] || (!durationsFetched.has(id) && cache[id].items.some(it => !it.duration_secs && !it.duration));
  if (needFetch) {
    durationsFetched.add(id);
    const pl = await call("get_playlist", { playlistId: id });
    if (pl) {
      cache[id] = pl;
      summaries = await call("list_summaries");
    } else {
      expanded.delete(id);
    }
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

listen("playlist-id-changed", async (e) => {
  const { oldId, newId } = e.payload;
  if (cache[oldId]) {
    cache[newId] = cache[oldId];
    cache[newId].playlist_id = newId;
    delete cache[oldId];
  }
  if (expanded.has(oldId)) {
    expanded.delete(oldId);
    expanded.add(newId);
  }
  try {
    summaries = await invoke("list_summaries");
  } catch (_) {}
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

function isChannelInput(text) {
  if (!text) return false;
  const s = text.trim();
  if (/[?&]list=/.test(s)) return false;
  if (/[?&]v=/.test(s) || /youtu\.be\//.test(s)) return false;
  if (s.startsWith("@") || /\/@/.test(s)) return true;
  if (/youtube\.com\/(channel|c|user)\//.test(s)) return true;
  if (/^(UC|HC)[A-Za-z0-9_-]{20,}$/.test(s)) return true;
  return false;
}

async function openChannel(text) {
  status("チャンネルの再生リストを取得中...");
  try {
    const res = await Common.openChannelPlaylists(text);
    if (res && res.updatedSummaries) {
      summaries = res.updatedSummaries;
      renderTree();
    }
    $("plInput").value = "";
    status("");
  } catch (e) {
    status("");
    alert(e);
  }
}

async function openChannelForVideo(video) {
  status("チャンネルの再生リストを取得中...");
  try {
    const res = await Common.openChannelForVideo(video);
    if (res && res.updatedSummaries) {
      summaries = res.updatedSummaries;
      renderTree();
    }
    status("");
  } catch (e) {
    status("");
    alert(e);
  }
}

$("addBtn").onclick = async () => {
  const text = $("plInput").value.trim();
  if (!text) return;
  if (isChannelInput(text)) {
    await openChannel(text);
    return;
  }
  // list= を含む URL / ID はプレイリストとして追加。それ以外の動画の URL は再生のみ
  const isPlaylist = /[?&]list=/.test(text) || /^(PL|UU|FL|OL|LL)[A-Za-z0-9_-]{10,}$/.test(text);
  const v = isPlaylist ? null : Common.parseVideoInput(text);
  if (v) { await playSingle(v); return; }
  const list = await call("add_playlist", { playlistInput: text });
  if (list) { summaries = list; $("plInput").value = ""; renderTree(); }
};

$("channelBtn").onclick = async () => {
  let text = $("plInput").value.trim();
  if (!text || (!isChannelInput(text) && (/[?&]list=/.test(text) || Common.parseVideoInput(text)))) {
    text = await Common.askText("チャンネルの URL または @ハンドル名を入力してください:\n（例: https://www.youtube.com/@channel名 または @channel名）");
    if (!text) return;
  }
  await openChannel(text);
};

document.addEventListener("keydown", (e) => {
  if (e.key !== "Delete" || !selected.id || /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement.tagName) || document.querySelector("dialog[open]")) return;
  removeSelected(selected.id, null);
});

$("copyBtn").onclick = async () => {
  const text = $("plInput").value.trim();
  if (isChannelInput(text)) {
    await openChannel(text);
    return;
  }
  if (!text) { status("開く公開リストの URL を入力してください"); return; }
  if (!/[?&]list=/.test(text) && !/^(PL|UU|FL|OL|LL)[A-Za-z0-9_-]{10,}$/.test(text)) { status("プレイリストの URL を入力してください"); return; }
  const r = await Common.openPublicList(text);
  if (!r) return;
  summaries = await invoke("list_summaries");
  if (cache[r.id]) cache[r.id] = await invoke("get_playlist", { playlistId: r.id });
  $("plInput").value = "";
  renderTree();
  refreshPush();
  status(`${r.added}本を「${r.title}」にコピーしました` + (r.skipped ? `（すでに入っている ${r.skipped}本はスキップ）` : "") + "。Push で YouTube に反映されます");
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
        if (e.data === YT.PlayerState.ENDED && (!playOnce || settings.repeat === "one")) advance(1, true);
        // 次の動画でも選択中の速度を維持する
        if (e.data === YT.PlayerState.PLAYING && player.getPlaybackRate() !== rate) player.setPlaybackRate(rate);
        updatePlayIcons();
      },
      onError: () => { if (!playOnce) advance(1, true, true); },   // 再生不可動画はスキップ（1本だけ再生のときは進まない。単一動画リピートでも同じ動画を繰り返さない）
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
  // 戻る操作のために履歴を残す（シャッフル中の「前へ」で使う）
  if (navBack) navBack = false;
  else if (nowPlaylistId && nowVideoId !== null && !(nowPlaylistId === playlistId && nowVideoId === videoId)) {
    history.push({ pl: nowPlaylistId, vid: nowVideoId });
    if (history.length > 200) history.shift();
  }
  if (playlistId) bag.add(key(playlistId, videoId));
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
  setTimeout(() => {
    document.querySelector(".vlist li.playing")?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, 60);
}

// ---- リピート / シャッフル ----
const REPEATS = [
  { v: "off", mark: "➡", name: "リピートなし" },
  { v: "all", mark: "🌐🔁", name: "全リストリピート" },
  { v: "list", mark: "📃🔁", name: "単一リストリピート" },
  { v: "one", mark: "🔂", name: "単一動画リピート" },
];
const SHUFFLES = [
  { v: "off", mark: "➡", name: "シャッフルなし" },
  { v: "all", mark: "🌐🔀", name: "全リストシャッフル" },
  { v: "list", mark: "📃🔀", name: "単一リストシャッフル" },
];
let bag = new Set();      // シャッフルで今回の周回にすでに再生した動画（全部再生するまで重複させない）
let history = [];         // 再生した順（シャッフル中の「前へ」用）
let navBack = false;
const key = (pl, vid) => `${pl}|${vid}`;

function shufflePool() {
  const lists = settings.shuffle === "all" ? Object.values(cache) : (cache[nowPlaylistId] ? [cache[nowPlaylistId]] : []);
  return lists.flatMap((p) => p.items.map((i) => ({ pl: p.playlist_id, vid: i.video_id })));
}

function pickShuffle() {
  const pool = shufflePool();
  if (!pool.length) return null;
  const fresh = () => pool.filter((x) => !bag.has(key(x.pl, x.vid)));
  let cand = fresh();
  if (!cand.length) {
    if (settings.repeat === "off") return null;   // リピートなしなら、全部再生したら終わる
    bag.clear();
    if (nowPlaylistId && nowVideoId !== null) bag.add(key(nowPlaylistId, nowVideoId));
    cand = fresh();
    if (!cand.length) cand = pool;   // 1本しかないとき
  }
  return cand[Math.floor(Math.random() * cand.length)];
}

// 次（d=1）/ 前（d=-1）に再生する動画。無ければ null（停止）
function nextTarget(d) {
  if (settings.shuffle !== "off") {
    if (d < 0) return history.length ? { back: true, ...history[history.length - 1] } : null;
    return pickShuffle();
  }
  const q = queue();
  if (!q.length) return null;
  const j = currentIndex() + d;
  if (j >= 0 && j < q.length) return { pl: nowPlaylistId, vid: q[j].video_id };
  if (settings.repeat === "all") {
    const order = summaries.map((s) => s.id).filter((id) => cache[id] && cache[id].items.length);
    const at = order.indexOf(nowPlaylistId);
    if (at >= 0) {
      const its = cache[order[(at + d + order.length) % order.length]].items;
      return { pl: order[(at + d + order.length) % order.length], vid: (d > 0 ? its[0] : its[its.length - 1]).video_id };
    }
  }
  if (settings.repeat === "list" || settings.repeat === "one") return { pl: nowPlaylistId, vid: q[(j + q.length) % q.length].video_id };
  return null;
}

// auto: 再生が終わって自動で進むとき。ignoreOne: 単一動画リピートを無視する（再生エラー時）
function advance(d, auto = false, ignoreOne = false) {
  if (nowVideoId === null) return;
  if (auto && settings.repeat === "one" && !ignoreOne) { playVideo(nowPlaylistId, nowVideoId, 0, playOnce); return; }
  const t = nextTarget(d);
  if (!t) { if (auto) status("最後まで再生しました"); return; }
  if (t.back) { history.pop(); navBack = true; }
  playVideo(t.pl, t.vid);
}

$("prevBtn").onclick = () => advance(-1);
$("nextBtn").onclick = () => advance(1);
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
const settings = Object.assign({ mode: "video", loop: true, shuffle: "off" },
  JSON.parse(localStorage.getItem("settings") || "{}"));
if (!settings.repeat) settings.repeat = settings.loop === false ? "off" : "list";   // 旧設定「ループ」からの引き継ぎ
const saveSettings = () => localStorage.setItem("settings", JSON.stringify(settings));

setMode(settings.mode === "music");
$("setMode").value = settings.mode;
$("setMode").onchange = () => { settings.mode = $("setMode").value; saveSettings(); setMode(settings.mode === "music"); };

// ---- 動画下部: リピート / シャッフルのプルダウン ----
function updatePlayModeUi() {
  const r = REPEATS.find((x) => x.v === settings.repeat) || REPEATS[0];
  const s = SHUFFLES.find((x) => x.v === settings.shuffle) || SHUFFLES[0];
  $("repeatBtn").textContent = `${r.mark} ${r.name} ▾`;
  $("shuffleBtn").textContent = `${s.mark} ${s.name} ▾`;
  $("repeatBtn").classList.toggle("active", r.v !== "off");
  $("shuffleBtn").classList.toggle("active", s.v !== "off");
}

// 「全リスト」は、読み込み済み（ツリーで開いたことがある）のリストだけが対象。未取得のリストは含めない
const loadedCount = () => Object.values(cache).filter((p) => p.items.length).length;
const scopeNote = () => status(`全リストの対象: 読み込み済みの${loadedCount()}リスト（未取得のリストは含まれません）`);

function openModeMenu(btn, options, current, apply) {
  const r = btn.getBoundingClientRect();
  Common.showMenu(r.left, r.top - 4, options.map((o) => ({
    label: `${o.v === current ? "✓" : "\u3000"} ${o.mark} ${o.name}`,
    fn: () => apply(o.v),
  })), true);
}

$("repeatBtn").onclick = () => openModeMenu($("repeatBtn"), REPEATS, settings.repeat, (v) => {
  settings.repeat = v;
  saveSettings();
  updatePlayModeUi();
  if (v === "all") scopeNote();
});
$("shuffleBtn").onclick = () => openModeMenu($("shuffleBtn"), SHUFFLES, settings.shuffle, (v) => {
  settings.shuffle = v;
  saveSettings();
  bag.clear();
  if (nowPlaylistId && nowVideoId !== null) bag.add(key(nowPlaylistId, nowVideoId));
  updatePlayModeUi();
  if (v === "all") scopeNote();
});
updatePlayModeUi();

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

const escapeHtml = (s) => String(s || "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

// ---- 重複チェック（ハンバーガーメニュー内） ----
let dedupGroups = [];

async function openDedupModal(initialMode = "single", initialA = null, initialB = null) {
  const dlg = $("dedupDlg");
  const modeSel = $("dedupMode");
  const singleControls = $("dedupSingleControls");
  const compareControls = $("dedupCompareControls");
  const allControls = $("dedupAllControls");

  const targetSel = $("dedupTarget");
  const targetSelA = $("dedupTargetA");
  const targetSelB = $("dedupTargetB");
  const swapBtn = $("dedupSwapBtn");

  const listEl = $("dedupList");
  const summaryText = $("dedupSummaryText");
  const delBtn = $("dedupDeleteBtn");

  function populateSelect(sel, includeAll = false) {
    sel.innerHTML = "";
    if (includeAll) {
      const optAll = document.createElement("option");
      optAll.value = "";
      optAll.textContent = "すべてのプレイリスト";
      sel.appendChild(optAll);
    }
    for (const s of summaries) {
      const opt = document.createElement("option");
      opt.value = s.id;
      opt.textContent = `${s.title} (${s.item_count}本)`;
      sel.appendChild(opt);
    }
  }

  populateSelect(targetSel, true);
  populateSelect(targetSelA, false);
  populateSelect(targetSelB, false);

  modeSel.value = initialMode;
  if (initialA && summaries.some((s) => s.id === initialA)) {
    targetSel.value = initialA;
    targetSelA.value = initialA;
  } else if (selected.id && summaries.some((s) => s.id === selected.id)) {
    targetSel.value = selected.id;
    targetSelA.value = selected.id;
  }

  if (initialB && summaries.some((s) => s.id === initialB)) {
    targetSelB.value = initialB;
  } else {
    const other = summaries.find((s) => s.id !== targetSelA.value);
    if (other) targetSelB.value = other.id;
  }

  function updateModeUi() {
    const m = modeSel.value;
    singleControls.style.display = (m === "single") ? "inline-flex" : "none";
    compareControls.style.display = (m === "compare") ? "inline-flex" : "none";
    allControls.style.display = (m === "all") ? "inline-flex" : "none";
  }
  updateModeUi();

  modeSel.onchange = () => {
    updateModeUi();
    renderDuplicates();
  };

  swapBtn.onclick = () => {
    const tmp = targetSelA.value;
    targetSelA.value = targetSelB.value;
    targetSelB.value = tmp;
    renderDuplicates();
  };

  targetSel.onchange = renderDuplicates;
  targetSelA.onchange = renderDuplicates;
  targetSelB.onchange = renderDuplicates;
  $("dedupScanBtn").onclick = renderDuplicates;

  function getCheckedCount() {
    return listEl.querySelectorAll("input.dedupCb:checked").length;
  }

  function updateCount() {
    const n = getCheckedCount();
    delBtn.disabled = (n === 0);
    delBtn.textContent = `選択した動画を削除 (${n}件)`;
  }

  async function ensurePlaylistLoaded(pid) {
    if (!pid) return;
    if (!cache[pid]) {
      cache[pid] = await call("get_playlist", { playlistId: pid });
    }
  }

  async function renderDuplicates() {
    listEl.innerHTML = "";
    delBtn.disabled = true;
    delBtn.textContent = "選択した動画を削除 (0件)";
    summaryText.textContent = "検索中...";

    const m = modeSel.value;
    let dedupGroups = [];

    try {
      if (m === "single") {
        const targetId = targetSel.value;
        if (targetId) await ensurePlaylistLoaded(targetId);
        dedupGroups = await invoke("find_duplicates", { playlistId: targetId || null });
      } else if (m === "compare") {
        const a = targetSelA.value;
        const b = targetSelB.value;
        if (!a || !b) {
          summaryText.textContent = "";
          listEl.innerHTML = '<div class="pkMsg">比較するプレイリストを2つ選択してください。</div>';
          return;
        }
        if (a === b) {
          summaryText.textContent = "";
          listEl.innerHTML = '<div class="pkMsg">基準リストAと比較先リストBには異なるプレイリストを選択してください。</div>';
          return;
        }
        await Promise.all([ensurePlaylistLoaded(a), ensurePlaylistLoaded(b)]);
        dedupGroups = await invoke("compare_duplicates", { playlistIds: [a, b] });
      } else if (m === "all") {
        for (const s of summaries) {
          await ensurePlaylistLoaded(s.id);
        }
        dedupGroups = await invoke("compare_duplicates", { playlistIds: [] });
      }
    } catch (e) {
      summaryText.textContent = "";
      listEl.innerHTML = `<div class="pkMsg">${escapeHtml(String(e))}</div>`;
      return;
    }

    if (!dedupGroups || dedupGroups.length === 0) {
      summaryText.textContent = "重複なし";
      listEl.innerHTML = '<div class="pkMsg">重複している動画は見つかりませんでした 🎉</div>';
      return;
    }

    let totalDupeItems = 0;
    for (const g of dedupGroups) {
      totalDupeItems += g.items.filter(it => it.is_duplicate).length;
    }
    const modeLabel = (m === "compare") ? "共通動画" : "重複";
    summaryText.textContent = `${dedupGroups.length} 本の${modeLabel} (削除対象候補: ${totalDupeItems}件)`;

    for (const g of dedupGroups) {
      const gBox = document.createElement("div");
      gBox.className = "dedupGroup";

      const thumb = document.createElement("img");
      thumb.className = "dedupThumb";
      thumb.src = g.thumbnail_url || "data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg'/>";
      thumb.title = "クリックで再生";
      thumb.onclick = () => { playVideo(g.playlist_id, g.video_id); };

      const details = document.createElement("div");
      details.className = "dedupDetails";

      const titleDiv = document.createElement("div");
      titleDiv.className = "dedupTitle";
      titleDiv.textContent = g.title;
      titleDiv.title = g.title;

      const metaDiv = document.createElement("div");
      metaDiv.className = "dedupMeta";
      metaDiv.innerHTML = `
        <span class="dedupChannel">${escapeHtml(g.channel || "")}</span>
        <span class="badge dupe">${g.items.length} 箇所に存在</span>
        <button class="dedupPlayBtn">▶ 再生</button>
      `;
      metaDiv.querySelector(".dedupPlayBtn").onclick = () => { playVideo(g.playlist_id, g.video_id); };

      const itemsDiv = document.createElement("div");
      itemsDiv.className = "dedupItems";

      for (const it of g.items) {
        const lbl = document.createElement("label");
        lbl.className = `dedupItemLabel ${it.is_duplicate ? "isDupe" : "isOrig"}`;

        const cb = document.createElement("input");
        cb.type = "checkbox";
        cb.className = "dedupCb";
        cb.dataset.pid = it.playlist_id || g.playlist_id;
        cb.dataset.id = it.playlist_item_id;
        cb.dataset.isDupe = it.is_duplicate ? "1" : "0";
        cb.checked = it.is_duplicate;
        cb.onchange = updateCount;

        const plTag = document.createElement("span");
        plTag.className = "dedupPlTag";
        plTag.textContent = it.playlist_title || g.playlist_title || "リスト";
        plTag.title = it.playlist_title || g.playlist_title || "";

        const badge = document.createElement("span");
        badge.className = `badge ${it.is_duplicate ? "badgeDupe" : "badgeOrig"}`;
        if (m === "compare") {
          badge.textContent = it.is_duplicate ? "重複 (削除対象)" : "基準A (保持)";
        } else if (m === "all") {
          badge.textContent = it.is_duplicate ? "重複" : "1件目 (保持)";
        } else {
          badge.textContent = it.is_duplicate ? "重複" : "1件目 (保持)";
        }

        const pos = document.createElement("span");
        pos.className = "dedupPos";
        pos.textContent = `位置 #${it.position + 1}`;

        lbl.append(cb, plTag, badge, pos);
        itemsDiv.appendChild(lbl);
      }

      details.append(titleDiv, metaDiv, itemsDiv);
      gBox.append(thumb, details);
      listEl.appendChild(gBox);
    }
    updateCount();
  }

  $("dedupSelDupes").onclick = () => {
    listEl.querySelectorAll("input.dedupCb").forEach((cb) => {
      cb.checked = (cb.dataset.isDupe === "1");
    });
    updateCount();
  };
  $("dedupSelAll").onclick = () => {
    listEl.querySelectorAll("input.dedupCb").forEach((cb) => {
      cb.checked = true;
    });
    updateCount();
  };
  $("dedupSelClear").onclick = () => {
    listEl.querySelectorAll("input.dedupCb").forEach((cb) => {
      cb.checked = false;
    });
    updateCount();
  };

  delBtn.onclick = async () => {
    const checked = Array.from(listEl.querySelectorAll("input.dedupCb:checked"));
    if (checked.length === 0) return;
    if (!confirm(`選択した ${checked.length} 件の動画をプレイリストから削除しますか？\n（YouTube 側への反映はメイン画面の「Push」で行われます）`)) return;

    const map = new Map();
    for (const cb of checked) {
      const pid = cb.dataset.pid;
      const id = cb.dataset.id;
      if (!map.has(pid)) map.set(pid, []);
      map.get(pid).push(id);
    }

    const targets = [];
    for (const [pid, itemIds] of map.entries()) {
      targets.push({ playlist_id: pid, playlist_item_ids: itemIds });
    }

    delBtn.disabled = true;
    try {
      const totalRemoved = await invoke("remove_selected_duplicates", { targets });
      for (const pid of map.keys()) {
        try {
          cache[pid] = await invoke("get_playlist", { playlistId: pid });
        } catch (_) {}
      }
      summaries = await invoke("list_summaries");
      renderTree();
      refreshPush();
      alert(`${totalRemoved} 件の重複動画を削除しました。`);
      await renderDuplicates();
    } catch (e) {
      alert(`削除中にエラーが発生しました: ${e}`);
      updateCount();
    }
  };

  $("dedupCloseBtn").onclick = () => { dlg.close(); };

  dlg.showModal();
  await renderDuplicates();
}

$("openDedup").onclick = () => { menu.hidden = true; openDedupModal(); };
$("openShorts").onclick = async () => {
  menu.hidden = true;
  if (!confirm("開いているプレイリストからショート動画（60秒以下）を一括削除しますか？\n※YouTube への反映は Push で行います。")) return;
  const count = await call("remove_shorts", {});
  if (typeof count === "number") {
    summaries = await call("list_summaries");
    for (const s of summaries) {
      if (expanded.has(s.id)) {
        cache[s.id] = await call("get_playlist", { playlistId: s.id });
      }
    }
    renderTree();
    refreshPush();
    alert(count > 0
      ? `計 ${count} 本のショート動画（60秒以下）を削除しました。`
      : "ショート動画はありませんでした。");
  }
};
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
$("musicChannelBtn").onclick = () => {
  if (nowVideoId) openChannelForVideo({ video_id: nowVideoId, ...(nowSingle || {}) });
  else status("動画が再生されていません");
};

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
  if (summaries.length) status("前回取得した内容を表示しています（最新にするには「マイリスト取得」、リストは右クリック「YouTubeから最新を取得」）");
  setTimeout(offerDeferred, 300);
})();
