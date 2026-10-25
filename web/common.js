// メイン画面と並べ替えウィンドウの共通部品（名前入力・右クリックメニュー・保存先リストの選択）
// 注意: YT は YouTube IFrame API が使うグローバル名なので、ここでは Common を使う。
const Common = (() => {
  const invoke = window.__TAURI__.core.invoke;

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  // ---- 名前入力ダイアログ（Promise<string|null>） ----
  function askText(title, placeholder, okLabel = "作成") {
    return new Promise((resolve) => {
      const dlg = el("dialog");
      dlg.appendChild(el("h3", "", title));
      const input = el("input");
      input.type = "text";
      input.maxLength = 150;
      input.placeholder = placeholder || "";
      dlg.appendChild(input);
      const btns = el("div", "dlgBtns");
      const ok = el("button", "primary", okLabel);
      const cancel = el("button", "", "キャンセル");
      btns.append(ok, cancel);
      dlg.appendChild(btns);
      document.body.appendChild(dlg);
      const finish = (v) => { dlg.close(); dlg.remove(); resolve(v); };
      ok.onclick = () => { const v = input.value.trim(); if (v) finish(v); else input.focus(); };
      cancel.onclick = () => finish(null);
      input.addEventListener("keydown", (e) => { if (e.key === "Enter") ok.click(); });
      dlg.addEventListener("cancel", (e) => { e.preventDefault(); finish(null); });   // Esc
      dlg.showModal();
      input.focus();
    });
  }

  // ---- 右クリックメニュー ----
  let menu = null;
  function hideMenu() { if (menu) { menu.remove(); menu = null; } }
  // up: true なら y をメニューの下端として、上向きに開く（画面下部のボタン用）
  function showMenu(x, y, items, up = false) {
    hideMenu();
    menu = el("div", "ctx");
    for (const it of items) {
      const b = el("button", it.danger ? "danger-text" : "", it.label);
      b.onclick = () => { hideMenu(); it.fn(); };
      menu.appendChild(b);
    }
    document.body.appendChild(menu);
    menu.style.left = `${Math.max(0, Math.min(x, window.innerWidth - menu.offsetWidth - 4))}px`;
    const top = up ? y - menu.offsetHeight : y;
    menu.style.top = `${Math.max(0, Math.min(top, window.innerHeight - menu.offsetHeight - 4))}px`;
    setTimeout(() => document.addEventListener("click", hideMenu, { once: true }), 0);
  }
  document.addEventListener("keydown", (e) => { if (e.key === "Escape") hideMenu(); });
  window.addEventListener("blur", hideMenu);

  // ---- 保存先リストの選択（複数チェック・新規リスト作成） ----
  // video: { video_id, title, thumbnail_url }。適用は手元のデータだけを変更し、YouTube へは Push で反映される。
  async function openPicker(video) {
    const dlg = el("dialog");
    dlg.id = "pickerDlg";
    dlg.appendChild(el("h3", "", "リストに保存"));
    dlg.appendChild(el("div", "pkVideo", video.title));
    const list = el("div", "pkList");
    list.appendChild(el("div", "pkMsg", "リストを読み込み中..."));
    dlg.appendChild(list);

    const newBox = el("div", "pkNew");
    const newInput = el("input");
    newInput.type = "text";
    newInput.maxLength = 150;
    newInput.placeholder = "新しいリスト名";
    const newBtn = el("button", "", "＋ 作成");
    newBox.append(newInput, newBtn);
    dlg.appendChild(newBox);

    dlg.appendChild(el("p", "note", "チェックを外すと、そのリストから取り除きます。YouTube への反映は Push で行います（作成・追加・削除は 50 ユニット/件）。"));
    const btns = el("div", "dlgBtns");
    const apply = el("button", "primary", "適用");
    const cancel = el("button", "", "キャンセル");
    btns.append(apply, cancel);
    dlg.appendChild(btns);
    document.body.appendChild(dlg);

    const rows = new Map();   // id -> { cb, initial }
    const close = () => { dlg.close(); dlg.remove(); };
    cancel.onclick = close;
    dlg.addEventListener("cancel", (e) => { e.preventDefault(); close(); });

    function addRow(summary, initial, top) {
      const row = el("label", "pkRow");
      const cb = el("input");
      cb.type = "checkbox";
      cb.checked = initial;
      const t = el("span", "t", summary.title);
      row.append(cb, t);
      if (summary.is_new) row.appendChild(el("span", "badge", "新規"));
      row.appendChild(el("span", "c", `${summary.item_count}本`));
      rows.set(summary.id, { cb, initial });
      if (top) list.insertBefore(row, list.firstChild); else list.appendChild(row);
    }

    async function containsVideo(id) {
      const pl = await invoke("get_playlist", { playlistId: id });
      return pl.items.some((i) => i.video_id === video.video_id);
    }

    newBtn.onclick = async () => {
      const name = newInput.value.trim();
      if (!name) { newInput.focus(); return; }
      newBtn.disabled = true;
      try {
        const before = new Set(rows.keys());
        const all = await invoke("create_playlist", { title: name });
        const created = all.find((s) => !before.has(s.id) && s.is_new);
        if (created) { addRow(created, false, true); rows.get(created.id).cb.checked = true; }
        newInput.value = "";
      } catch (e) { alert(e); }
      newBtn.disabled = false;
    };
    newInput.addEventListener("keydown", (e) => { if (e.key === "Enter") newBtn.click(); });

    apply.onclick = async () => {
      const add = [], remove = [];
      for (const [id, r] of rows) {
        if (r.cb.checked && !r.initial) add.push(id);
        if (!r.cb.checked && r.initial) remove.push(id);
      }
      if (!add.length && !remove.length) { close(); return; }
      apply.disabled = true;
      try {
        await invoke("set_video_playlists", {
          video: { video_id: video.video_id, title: video.title, thumbnail_url: video.thumbnail_url || "" },
          add, remove,
        });
        close();
      } catch (e) { alert(e); apply.disabled = false; }
    };

    dlg.showModal();

    // 一覧を読み込む。未取得のリストは動画一覧を取得して、すでに入っているかを調べる（1ユニット/50件）
    try {
      const summaries = await invoke("list_summaries");
      list.textContent = "";
      if (!summaries.length) list.appendChild(el("div", "pkMsg", "リストがありません。下から作成できます。"));
      let n = 0;
      for (const s of summaries) {
        n += 1;
        apply.disabled = true;
        try {
          addRow(s, await containsVideo(s.id), false);
        } catch (e) {
          const row = el("div", "pkRow pkErr", `${s.title}（取得できませんでした）`);
          list.appendChild(row);
        }
        apply.disabled = false;
      }
    } catch (e) {
      list.textContent = "";
      list.appendChild(el("div", "pkMsg", String(e)));
      apply.disabled = false;
    }
  }

  // ---- 他のリストの動画を、target リストの末尾へ取り込む（コピー）。結果 {added, skipped}、取り消しは null ----
  async function openImporter(target) {
    const dlg = el("dialog");
    dlg.id = "importDlg";
    dlg.appendChild(el("h3", "", "他のリストの動画を取り込む"));
    dlg.appendChild(el("div", "pkVideo", `取り込み先: ${target.title}`));
    const list = el("div", "pkList");
    list.appendChild(el("div", "pkMsg", "リストを読み込み中..."));
    dlg.appendChild(list);
    dlg.appendChild(el("p", "note", "選んだリストの動画を、このリストの末尾にコピーします（すでに入っている動画はスキップ・元のリストは変わりません）。YouTube への反映は Push で行います（1本 50 ユニット）。"));
    const btns = el("div", "dlgBtns");
    const apply = el("button", "primary", "取り込む");
    const cancel = el("button", "", "キャンセル");
    btns.append(apply, cancel);
    dlg.appendChild(btns);
    document.body.appendChild(dlg);

    let finish;
    const result = new Promise((r) => { finish = r; });
    const close = (v) => { dlg.close(); dlg.remove(); finish(v); };
    cancel.onclick = () => close(null);
    dlg.addEventListener("cancel", (e) => { e.preventDefault(); close(null); });

    const rows = new Map();   // id -> checkbox
    apply.onclick = async () => {
      const sources = [...rows].filter(([, cb]) => cb.checked).map(([id]) => id);
      if (!sources.length) { close(null); return; }
      apply.disabled = true;
      cancel.disabled = true;
      try {
        // 未取得のリストは動画一覧を取得する（1ユニット/50件。取得済みなら通信しない）
        for (const id of sources) await invoke("get_playlist", { playlistId: id });
        close(await invoke("import_playlists", { target: target.id, sources }));
      } catch (e) {
        alert(e);
        apply.disabled = false;
        cancel.disabled = false;
      }
    };
    dlg.showModal();

    try {
      const others = (await invoke("list_summaries")).filter((s) => s.id !== target.id);
      list.textContent = "";
      if (!others.length) list.appendChild(el("div", "pkMsg", "取り込めるリストがありません。"));
      for (const s of others) {
        const row = el("label", "pkRow");
        const cb = el("input");
        cb.type = "checkbox";
        rows.set(s.id, cb);
        row.append(cb, el("span", "t", s.title));
        if (s.is_new) row.appendChild(el("span", "badge", "新規"));
        row.appendChild(el("span", "c", `${s.item_count}本`));
        list.appendChild(row);
      }
    } catch (e) {
      list.textContent = "";
      list.appendChild(el("div", "pkMsg", String(e)));
    }
    return result;
  }


  // ---- リスト内の検索・並べ替えのポップアップ。項目を選ぶと onPick(item, 位置) を呼んで閉じる ----
  async function openFinder(playlistId, onPick) {
    const dlg = el("dialog");
    dlg.id = "finderDlg";
    const head = el("h3", "", "検索・並べ替え");
    const input = el("input");
    input.type = "search";
    input.placeholder = "タイトル・チャンネル名で検索（スペース区切りで AND）";
    const bar = el("div", "fdBar");
    const sel = el("select");
    for (const [v, t] of [["original", "現在の順"], ["title", "名前順"], ["channel", "チャンネル順"]]) {
      const o = el("option", "", t);
      o.value = v;
      sel.appendChild(o);
    }
    const dirBtn = el("button", "", "昇順");
    const applyBtn = el("button", "primary", "この並びでリストを並べ替え");
    bar.append(el("span", "", "並べ替え:"), sel, dirBtn, applyBtn);
    const info = el("div", "fdInfo", "読み込み中...");
    const list = el("div", "pkList fdList");
    const btns = el("div", "dlgBtns");
    const closeBtn = el("button", "", "閉じる");
    btns.appendChild(closeBtn);
    dlg.append(head, input, bar, info, list, btns);
    document.body.appendChild(dlg);

    const close = () => { dlg.close(); dlg.remove(); };
    closeBtn.onclick = close;
    dlg.addEventListener("cancel", (e) => { e.preventDefault(); close(); });

    let items = [];
    let desc = false;
    const cmp = (a, b) => a.localeCompare(b, "ja", { sensitivity: "base", numeric: true });
    const sorted = () => {
      const rows = items.map((it, idx) => ({ it, idx }));
      const k = sel.value;
      if (k === "title") rows.sort((x, y) => cmp(x.it.title, y.it.title));
      else if (k === "channel") rows.sort((x, y) => cmp(x.it.channel || "", y.it.channel || "") || cmp(x.it.title, y.it.title));
      if (desc && k !== "original") rows.reverse();
      return rows;
    };

    function draw() {
      const terms = input.value.toLowerCase().split(/\s+/).filter(Boolean);
      const rows = sorted().filter(({ it }) => {
        const hay = `${it.title} ${it.channel || ""}`.toLowerCase();
        return terms.every((t) => hay.includes(t));
      });
      list.textContent = "";
      info.textContent = terms.length ? `${rows.length}件 / 全${items.length}件` : `全${items.length}件`;
      if (!rows.length) list.appendChild(el("div", "pkMsg", "一致する動画がありません。"));
      for (const { it, idx } of rows) {
        const row = el("div", "pkRow");
        row.title = "クリックで選択";
        row.append(el("span", "no", String(idx + 1)), el("span", "t", it.title), el("span", "c", it.channel || "（チャンネル不明）"));
        row.onclick = () => { close(); onPick(it, idx); };
        list.appendChild(row);
      }
      dirBtn.disabled = sel.value === "original";
      applyBtn.disabled = sel.value === "original";
    }

    input.addEventListener("input", draw);
    sel.onchange = draw;
    dirBtn.onclick = () => { desc = !desc; dirBtn.textContent = desc ? "降順" : "昇順"; draw(); };
    applyBtn.onclick = async () => {
      applyBtn.disabled = true;
      try {
        await invoke("sort_items", { playlistId, key: sel.value, descending: desc });
        items = (await invoke("get_playlist", { playlistId })).items;
        sel.value = "original";
        desc = false;
        dirBtn.textContent = "昇順";
        draw();
      } catch (e) { alert(e); applyBtn.disabled = false; }
    };

    dlg.showModal();
    input.focus();
    try {
      const pl = await invoke("get_playlist", { playlistId });
      items = pl.items;
      head.textContent = `検索・並べ替え: ${pl.title}`;
      draw();
    } catch (e) {
      info.textContent = "";
      list.textContent = "";
      list.appendChild(el("div", "pkMsg", String(e)));
    }
  }


  // ---- Push ダイアログ: 件数・概算コスト・今日の残りを示し、足りないときは進め方を選ぶ ----
  const fmtDur = (secs) => {
    const h = Math.floor(secs / 3600), m = Math.floor((secs % 3600) / 60);
    return h ? `${h}時間${m}分` : `${m}分`;
  };
  const fmtTime = (unixSecs) => new Date(unixSecs * 1000).toLocaleString("ja-JP", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" });

  // ids: 対象のリスト id の配列（null なら変更のある全リスト）
  async function openPush(ids) {
    const plan = await invoke("push_plan", { playlistIds: ids });
    if (plan.running) { alert("すでに反映処理を実行中です。"); return; }
    if (!plan.ops) { alert("YouTube へ反映する変更はありません。"); return; }

    const dlg = el("dialog");
    dlg.id = "pushDlg";
    dlg.appendChild(el("h3", "", "YouTube に反映（Push）"));
    const body = el("div");
    dlg.appendChild(body);
    const btns = el("div", "dlgBtns");
    dlg.appendChild(btns);
    document.body.appendChild(dlg);

    let unlisten = null;
    const close = () => { if (unlisten) unlisten(); dlg.close(); dlg.remove(); };
    dlg.addEventListener("cancel", (e) => e.preventDefault());   // 実行中に Esc で閉じてしまわないように

    const resetAt = Math.floor(Date.now() / 1000) + plan.reset_in_secs;
    const ul = el("ul", "pushList");
    for (const p of plan.lists) ul.appendChild(el("li", "", p.deleting ? `${p.title}: リストを削除` : `${p.title}: ${p.count}件`));
    body.appendChild(ul);

    const tbl = el("table", "pushCost");
    const row = (k, v) => { const tr = el("tr"); tr.append(el("td", "", k), el("td", "", v)); tbl.appendChild(tr); };
    row("今回の変更", `${plan.ops}件`);
    row("概算コスト", `${plan.cost.toLocaleString()} ユニット（1件 50）`);
    row("今日の使用済み（推定）", `${plan.used.toLocaleString()} / ${plan.limit.toLocaleString()}`);
    row("今日の残り", `${plan.remaining.toLocaleString()} ユニット（${plan.affordable_ops}件ぶん）`);
    row("上限の回復", `${fmtTime(resetAt)} ごろ（あと${fmtDur(plan.reset_in_secs)}）`);
    body.appendChild(tbl);

    let chosen = "now";
    if (plan.fits) {
      body.appendChild(el("p", "pushOk", "今日の残りで全件を反映できます。"));
    } else {
      const short = plan.ops - plan.affordable_ops;
      body.appendChild(el("p", "pushWarn", `⚠ 今日の残りでは足りません（${short}件ぶん不足）。全件の反映には最短で${plan.days_needed}日かかります。進め方を選んでください。`));
      const opts = [
        ["defer", `今日の残り（${plan.affordable_ops}件）だけ反映し、続きは別日に行う`,
          "残りは保存され、次回アプリを起動したときに続きを反映できます。"],
        ["wait", "最小化して常駐し、終わるまで自動で反映する",
          `上限に達したらウィンドウを最小化して待機し、回復（${fmtTime(resetAt)}ごろ）後に自動で再開します。アプリは終了せずに起動したままにしてください（終了しても残りは保存され、次回起動時に続けられます）。`],
      ];
      chosen = "defer";
      for (const [v, label, note] of opts) {
        const lab = el("label", "pushOpt");
        const r = el("input");
        r.type = "radio";
        r.name = "pushMode";
        r.checked = v === chosen;
        r.onchange = () => { chosen = v; };
        const box = el("div");
        box.append(el("div", "", label), el("div", "note", note));
        lab.append(r, box);
        body.appendChild(lab);
      }
    }
    body.appendChild(el("p", "note", "※ 使用量はこのアプリの消費分の推定です（同じ Google Cloud プロジェクトの他の利用は含みません）。新規リストは非公開で作成されます。対象は「手動並べ替え」設定のプレイリストです。"));

    const go = el("button", "primary danger", "反映する");
    const cancel = el("button", "", "キャンセル");
    cancel.onclick = close;
    btns.append(go, cancel);

    go.onclick = async () => {
      // 進捗表示に切り替える
      body.textContent = "";
      const bar = el("progress");
      bar.max = plan.ops;
      bar.value = 0;
      const msg = el("p", "pushMsg", "反映中...");
      body.append(bar, msg);
      btns.textContent = "";
      const stop = el("button", "", "中止");
      stop.onclick = () => { stop.disabled = true; invoke("push_cancel"); };
      btns.appendChild(stop);

      const finishBtn = (text) => {
        btns.textContent = "";
        const ok = el("button", "primary", "閉じる");
        ok.onclick = close;
        btns.appendChild(ok);
        msg.textContent = text;
      };
      unlisten = await window.__TAURI__.event.listen("push-progress", (e) => {
        const p = e.payload;
        bar.max = Math.max(p.total, 1);
        bar.value = p.done;
        const prog = `${p.done} / ${p.total}件`;
        if (p.state === "running") msg.textContent = `反映中... ${prog}`;
        else if (p.state === "waiting") msg.textContent = `${prog} 反映しました。今日の上限に達したため待機中です。${fmtTime(p.resume_at)}ごろに自動で再開します（ウィンドウは最小化されます。アプリを終了しないでください）。`;
        else if (p.state === "done") finishBtn(`${prog} すべて反映しました。`);
        else if (p.state === "deferred") finishBtn(`${prog} 反映しました。残り${p.total - p.done}件は保存しました。次回アプリを起動したときに続きを反映できます。`);
        else if (p.state === "cancelled") finishBtn(`中止しました（${prog} 反映済み）。残りはもう一度 Push すると続きから反映します。`);
        else if (p.state === "error") finishBtn(`${p.message}\n（${prog} 反映済みです。もう一度 Push すると続きから反映します）`);
      });
      try {
        await invoke("start_push", { playlistIds: ids, mode: chosen });
      } catch (e) { finishBtn(String(e)); }
    };
    dlg.showModal();
  }

  // ---- 入力された URL から動画 ID と開始位置（秒）を取り出す。動画でなければ null ----
  // 対応: watch?v=ID / youtu.be/ID / shorts/ID / live/ID / embed/ID / music.youtube.com、開始位置 t= と start=、11文字の動画ID
  // list= を含む URL は、これまでどおりプレイリストとして扱う（呼び出し側で先に判定する）。
  function parseVideoInput(text) {
    const s = String(text || "").trim();
    if (/^[A-Za-z0-9_-]{11}$/.test(s)) return { videoId: s, start: 0 };
    let u;
    try { u = new URL(/^https?:\/\//i.test(s) ? s : `https://${s}`); } catch (_) { return null; }
    const host = u.hostname.toLowerCase().replace(/^www\./, "").replace(/^m\./, "");
    let id = null;
    if (host === "youtu.be") id = u.pathname.split("/")[1];
    else if (host === "youtube.com" || host === "music.youtube.com" || host === "youtube-nocookie.com") {
      if (u.pathname === "/watch") id = u.searchParams.get("v");
      else {
        const m = u.pathname.match(/^\/(?:shorts|live|embed|v)\/([^/?#]+)/);
        if (m) id = m[1];
      }
    }
    if (!id || !/^[A-Za-z0-9_-]{11}$/.test(id)) return null;
    return { videoId: id, start: parseStart(u.searchParams.get("t") || u.searchParams.get("start")) };
  }

  // "90" / "90s" / "1m30s" / "1h2m3s" → 秒
  function parseStart(v) {
    if (!v) return 0;
    if (/^\d+$/.test(v)) return Number(v);
    const m = v.match(/^(?:(\d+)h)?(?:(\d+)m)?(?:(\d+)s?)?$/);
    if (!m) return 0;
    return (Number(m[1] || 0) * 3600) + (Number(m[2] || 0) * 60) + Number(m[3] || 0);
  }

  return { askText, showMenu, hideMenu, openPicker, openImporter, openFinder, openPush, parseVideoInput };
})();
