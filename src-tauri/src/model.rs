//! ローカル管理のプレイリストモデル（API通信なし・クォータ消費なし）。
//!
//! 画面での編集（並べ替え・追加・削除・新規リスト）はすべてこのモデルに溜め、
//! Push のときに `plan()` が返す操作列を YouTube へ順に反映する。

use std::collections::HashSet;

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct PlaylistItem {
    /// 既存の動画は YouTube の playlistItem ID、新しく追加した動画は "new-N"。
    pub playlist_item_id: String,
    pub video_id: String,
    pub title: String,
    /// YouTube 側での位置（新規は現在位置と同じ値）。
    pub original_position: usize,
    pub current_position: usize,
    pub thumbnail_url: String,
    /// チャンネル名（取得できない動画は空）。
    #[serde(default)]
    pub channel: String,
    /// チャンネルID（UC... または空）。
    #[serde(default)]
    pub channel_id: String,
    /// YouTube 側にまだ無い（Push で追加される）。
    pub is_new: bool,
}

/// リストへ追加する動画の情報。
#[derive(Clone, Serialize, Deserialize)]
pub struct VideoInfo {
    pub video_id: String,
    pub title: String,
    pub thumbnail_url: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub channel_url: String,
}

/// Push で YouTube へ行う1回分の操作（いずれも 50 ユニット）。
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    CreatePlaylist,
    /// リストそのものを YouTube から削除する。
    DeletePlaylist,
    Delete { item_id: String },
    Insert { item_id: String, video_id: String, position: usize },
    Move { item_id: String, video_id: String, position: usize },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Playlist {
    /// アプリ内のキー。YouTube 上のリストは YouTube の ID、新規リストは "local-N"。
    pub playlist_id: String,
    /// YouTube 側の ID。まだ作成していない新規リストは None。
    pub remote_id: Option<String>,
    pub title: String,
    pub items: Vec<PlaylistItem>,
    /// YouTube から取得した時刻（UNIX 秒）。手元で作った新規リストは 0。
    #[serde(default)]
    pub fetched_at: i64,
    /// YouTube 側の現在の並び（playlist_item_id）。Push の各操作の成功ごとに更新する。
    #[serde(default)]
    server: Vec<String>,
    /// 画面から取り除いたが、まだ YouTube からは削除していない動画（「元に戻す」用）。
    #[serde(default)]
    removed: Vec<PlaylistItem>,
    /// 一覧から削除済み。Push で YouTube からも削除する。
    #[serde(default)]
    pub deleted: bool,
}

impl Playlist {
    pub fn new(playlist_id: String, title: String, mut items: Vec<PlaylistItem>) -> Self {
        items.sort_by_key(|i| i.original_position);
        let server = items.iter().map(|i| i.playlist_item_id.clone()).collect();
        let mut p = Playlist {
            remote_id: Some(playlist_id.clone()),
            playlist_id,
            title,
            items,
            fetched_at: crate::quota::now_secs(),
            server,
            removed: Vec::new(),
            deleted: false,
        };
        p.refresh();
        p
    }

    /// 手元だけで作る新規リスト。Push で YouTube に作成される。
    pub fn new_local(playlist_id: String, title: String) -> Self {
        Playlist { playlist_id, remote_id: None, title, items: Vec::new(), fetched_at: 0, server: Vec::new(), removed: Vec::new(), deleted: false }
    }

    /// YouTube 上の実際の並び（playlist_item_id の順）を取り込む。手元の動画の並びは変えない。
    pub fn set_server(&mut self, ids: Vec<String>) {
        self.server = ids;
        self.refresh();
    }

    /// YouTube 側にリストが見つからないとき: 未作成の状態に戻す（全動画が「新規」になり、次の Push で作り直される）。
    pub fn forget_remote(&mut self) {
        self.remote_id = None;
        self.server.clear();
        self.removed.clear();
        self.refresh();
    }

    /// 現在位置・サーバー上の位置・新規フラグを再計算する。
    fn refresh(&mut self) {
        let server = &self.server;
        for (idx, item) in self.items.iter_mut().enumerate() {
            item.current_position = idx;
            match server.iter().position(|id| *id == item.playlist_item_id) {
                Some(p) => {
                    item.original_position = p;
                    item.is_new = false;
                }
                None => {
                    item.original_position = idx;
                    item.is_new = true;
                }
            }
        }
    }

    pub fn sort_by_title(&mut self, descending: bool) {
        self.sort_by("title", descending);
    }

    /// key: "title"（名前順）/ "channel"（チャンネル順。同じチャンネル内は名前順）/ "original"（YouTube 上の並び。新規は末尾）。
    pub fn sort_by(&mut self, key: &str, descending: bool) {
        match key {
            "channel" => self.items.sort_by_cached_key(|i| (i.channel.to_lowercase(), i.title.to_lowercase())),
            "original" => self.items.sort_by_key(|i| (i.is_new, i.original_position)),
            _ => self.items.sort_by_cached_key(|i| i.title.to_lowercase()),
        }
        if descending {
            self.items.reverse();
        }
        self.refresh();
    }

    pub fn shuffle(&mut self) {
        self.items.shuffle(&mut rand::rng());
        self.refresh();
    }

    /// ドラッグ＆ドロップ用: from の要素を to へ移動。
    pub fn move_item(&mut self, from: usize, to: usize) -> Result<(), String> {
        let n = self.items.len();
        if from >= n || to >= n {
            return Err("index out of range".into());
        }
        let item = self.items.remove(from);
        self.items.insert(to, item);
        self.refresh();
        Ok(())
    }

    /// YouTube 側の状態に戻す（並びを戻し、取り除いた動画を復活させ、新しく追加した動画を取り消す）。
    pub fn reset(&mut self) {
        let server = self.server.clone();
        let mut all: Vec<PlaylistItem> =
            self.items.drain(..).filter(|i| server.contains(&i.playlist_item_id)).collect();
        all.append(&mut self.removed);
        all.sort_by_key(|i| server.iter().position(|id| *id == i.playlist_item_id).unwrap_or(usize::MAX));
        self.items = all;
        self.refresh();
    }

    pub fn contains_video(&self, video_id: &str) -> bool {
        self.items.iter().any(|i| i.video_id == video_id)
    }

    /// 動画を追加する（index が None なら末尾）。直前に取り除いた同じ動画があれば、それを復活させる。
    pub fn add_video(&mut self, video: VideoInfo, new_id: String, index: Option<usize>) {
        let item = match self.removed.iter().position(|i| i.video_id == video.video_id) {
            Some(pos) => self.removed.remove(pos),
            None => PlaylistItem {
                playlist_item_id: new_id,
                video_id: video.video_id,
                title: video.title,
                original_position: 0,
                current_position: 0,
                thumbnail_url: video.thumbnail_url,
                channel: video.channel,
                channel_id: String::new(),
                is_new: true,
            },
        };
        let at = index.unwrap_or(self.items.len()).min(self.items.len());
        self.items.insert(at, item);
        self.refresh();
    }

    fn drop_item(&mut self, pos: usize) -> PlaylistItem {
        let item = self.items.remove(pos);
        if self.server.contains(&item.playlist_item_id) {
            self.removed.push(item.clone());
        }
        item
    }

    /// 指定の動画をすべて取り除く。取り除いた件数を返す。
    pub fn remove_video(&mut self, video_id: &str) -> usize {
        let mut n = 0;
        while let Some(pos) = self.items.iter().position(|i| i.video_id == video_id) {
            self.drop_item(pos);
            n += 1;
        }
        self.refresh();
        n
    }

    /// 動画を1件取り出す（別のリストへ移すとき用）。
    pub fn take_item(&mut self, item_id: &str) -> Option<PlaylistItem> {
        let pos = self.items.iter().position(|i| i.playlist_item_id == item_id)?;
        let item = self.drop_item(pos);
        self.refresh();
        Some(item)
    }

    /// YouTube へ反映すべき操作を、実行順に返す。
    ///
    /// 1. 新規リストなら作成  2. 取り除いた動画の削除  3. 先頭から順に、新規動画は挿入、既存動画は位置の移動。
    /// 操作は1件ごとに他の動画の位置をずらすため、サーバー側の並びをシミュレートし、
    /// すでに目標位置にある動画はスキップする。確定済みの前方は後続の操作で乱れない。
    pub fn plan(&self) -> Vec<Op> {
        let mut ops = Vec::new();
        if self.deleted {
            if self.remote_id.is_some() {
                ops.push(Op::DeletePlaylist);
            }
            return ops;
        }
        if self.remote_id.is_none() {
            ops.push(Op::CreatePlaylist);
        }
        let target: HashSet<&str> = self.items.iter().map(|i| i.playlist_item_id.as_str()).collect();
        for id in self.server.iter().filter(|id| !target.contains(id.as_str())) {
            ops.push(Op::Delete { item_id: id.clone() });
        }
        let mut server: Vec<String> =
            self.server.iter().filter(|id| target.contains(id.as_str())).cloned().collect();
        for (t, item) in self.items.iter().enumerate() {
            if server.get(t).map(String::as_str) == Some(item.playlist_item_id.as_str()) {
                continue;
            }
            match server.iter().position(|id| *id == item.playlist_item_id) {
                Some(cur) => {
                    let id = server.remove(cur);
                    server.insert(t.min(server.len()), id);
                    ops.push(Op::Move {
                        item_id: item.playlist_item_id.clone(),
                        video_id: item.video_id.clone(),
                        position: t,
                    });
                }
                None => {
                    server.insert(t.min(server.len()), item.playlist_item_id.clone());
                    ops.push(Op::Insert {
                        item_id: item.playlist_item_id.clone(),
                        video_id: item.video_id.clone(),
                        position: t,
                    });
                }
            }
        }
        ops
    }

    /// 成功した操作を、サーバー側の並びに反映する。result_id は作成/挿入で YouTube が返した ID。
    pub fn apply(&mut self, op: &Op, result_id: Option<String>) {
        match op {
            Op::CreatePlaylist => self.remote_id = result_id,
            Op::DeletePlaylist => {} // 呼び出し側が、このリストを状態から取り除く
            Op::Delete { item_id } => {
                self.server.retain(|id| id != item_id);
                self.removed.retain(|i| &i.playlist_item_id != item_id);
            }
            Op::Insert { item_id, position, .. } => {
                let real = result_id.unwrap_or_else(|| item_id.clone());
                let at = (*position).min(self.server.len());
                self.server.insert(at, real.clone());
                if let Some(i) = self.items.iter_mut().find(|i| i.playlist_item_id == *item_id) {
                    i.playlist_item_id = real;
                }
            }
            Op::Move { item_id, position, .. } => {
                if let Some(cur) = self.server.iter().position(|id| id == item_id) {
                    let id = self.server.remove(cur);
                    let at = (*position).min(self.server.len());
                    self.server.insert(at, id);
                }
            }
        }
        self.refresh();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(k: usize) -> PlaylistItem {
        PlaylistItem {
            playlist_item_id: format!("i{k}"),
            video_id: format!("v{k}"),
            title: format!("T{k}"),
            original_position: k,
            current_position: k,
            thumbnail_url: String::new(),
            channel: format!("C{}", 5 - k % 3),
            channel_id: String::new(),
            is_new: false,
        }
    }

    fn make(n: usize) -> Playlist {
        Playlist::new("PL".into(), "T".into(), (0..n).map(item).collect())
    }

    fn video(k: usize) -> VideoInfo {
        VideoInfo { video_id: format!("x{k}"), title: format!("X{k}"), thumbnail_url: String::new(), channel: String::new(), channel_url: String::new() }
    }

    /// 実行時と同じく「先頭の操作を1件ずつ反映」を繰り返す。操作数を返す。
    fn run(pl: &mut Playlist) -> usize {
        let mut n = 0;
        while let Some(op) = pl.plan().into_iter().next() {
            let id = match &op {
                Op::CreatePlaylist => Some("remote-PL".to_string()),
                Op::Insert { item_id, .. } => Some(format!("srv-{item_id}")),
                _ => None,
            };
            pl.apply(&op, id);
            n += 1;
            assert!(n < 10_000, "plan does not converge");
        }
        n
    }

    fn ids(pl: &Playlist) -> Vec<String> {
        pl.items.iter().map(|i| i.playlist_item_id.clone()).collect()
    }

    #[test]
    fn deleted_list_plans_only_playlist_delete() {
        let mut pl = make(5);
        pl.deleted = true;
        assert_eq!(pl.plan(), vec![Op::DeletePlaylist]);
        let mut local = Playlist::new_local("local-1".into(), "x".into());
        local.deleted = true;
        assert!(local.plan().is_empty());
    }

    #[test]
    fn no_change_no_ops() {
        assert!(make(10).plan().is_empty());
    }

    #[test]
    fn single_move_is_one_op() {
        let mut pl = make(10);
        pl.move_item(9, 0).unwrap();
        assert_eq!(pl.plan().len(), 1);
    }

    #[test]
    fn random_orders_converge_to_target() {
        for n in 1..30 {
            for _ in 0..20 {
                let mut pl = make(n);
                pl.shuffle();
                let want = ids(&pl);
                let planned = pl.plan().len();
                assert_eq!(run(&mut pl), planned);
                assert_eq!(pl.server, want);
            }
        }
    }

    #[test]
    fn add_and_remove_converge() {
        let mut pl = make(6);
        assert_eq!(pl.remove_video("v1"), 1);
        assert_eq!(pl.remove_video("v4"), 1);
        pl.add_video(video(1), "new-1".into(), Some(0));
        pl.add_video(video(2), "new-2".into(), None);
        pl.shuffle();
        // 削除2 + 挿入2 + 並べ替え
        assert!(pl.plan().len() >= 4);
        run(&mut pl);
        assert_eq!(pl.server, ids(&pl));
        assert!(pl.items.iter().all(|i| !i.is_new));
        assert_eq!(pl.items.len(), 6);
    }

    #[test]
    fn re_adding_removed_video_restores_it() {
        let mut pl = make(3);
        pl.remove_video("v1");
        pl.add_video(VideoInfo { video_id: "v1".into(), title: "T1".into(), thumbnail_url: String::new(), channel: String::new(), channel_url: String::new() }, "new-1".into(), Some(1));
        assert!(pl.plan().is_empty(), "削除→再追加は何も送らない");
    }

    #[test]
    fn new_playlist_creates_then_inserts_in_order() {
        let mut pl = Playlist::new_local("local-1".into(), "New".into());
        pl.add_video(video(1), "new-1".into(), None);
        pl.add_video(video(2), "new-2".into(), None);
        pl.add_video(video(3), "new-3".into(), Some(0));
        let plan = pl.plan();
        assert_eq!(plan[0], Op::CreatePlaylist);
        assert_eq!(plan.len(), 4);
        let want: Vec<String> = ["x3", "x1", "x2"].iter().map(|s| s.to_string()).collect();
        run(&mut pl);
        assert_eq!(pl.remote_id.as_deref(), Some("remote-PL"));
        assert_eq!(pl.items.iter().map(|i| i.video_id.clone()).collect::<Vec<_>>(), want);
        assert_eq!(pl.server, ids(&pl));
    }

    #[test]
    fn empty_new_playlist_is_one_op() {
        assert_eq!(Playlist::new_local("local-1".into(), "N".into()).plan(), vec![Op::CreatePlaylist]);
    }

    #[test]
    fn sort_by_channel_and_original() {
        let mut pl = make(6);
        pl.sort_by("channel", false);
        let ch: Vec<_> = pl.items.iter().map(|i| i.channel.clone()).collect();
        let mut sorted = ch.clone();
        sorted.sort();
        assert_eq!(ch, sorted);
        pl.sort_by("channel", true);
        assert_eq!(pl.items[0].channel, "C5");
        pl.sort_by("original", false);
        let pos: Vec<_> = pl.items.iter().map(|i| i.original_position).collect();
        assert_eq!(pos, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn serde_roundtrip_keeps_pending_plan() {
        let mut pl = make(4);
        pl.move_item(0, 3).unwrap();
        pl.remove_video("v1");
        pl.add_video(video(9), "new-1".into(), None);
        let back: Playlist = serde_json::from_str(&serde_json::to_string(&pl).unwrap()).unwrap();
        assert_eq!(back.plan(), pl.plan());
        assert!(!pl.plan().is_empty());
    }

    #[test]
    fn take_item_moves_between_playlists() {
        let mut a = make(3);
        let mut b = make(2);
        let it = a.take_item("i1").unwrap();
        b.add_video(
            VideoInfo { video_id: it.video_id, title: it.title, thumbnail_url: it.thumbnail_url, channel: it.channel, channel_url: String::new() },
            "new-9".into(),
            Some(1),
        );
        assert_eq!(a.items.len(), 2);
        assert_eq!(a.plan(), vec![Op::Delete { item_id: "i1".into() }]);
        assert!(matches!(b.plan()[0], Op::Insert { position: 1, .. }));
    }

    #[test]
    fn reset_restores_server_state() {
        let mut pl = make(5);
        pl.remove_video("v2");
        pl.add_video(video(9), "new-1".into(), None);
        pl.shuffle();
        pl.reset();
        assert!(pl.plan().is_empty());
        assert_eq!(ids(&pl), (0..5).map(|k| format!("i{k}")).collect::<Vec<_>>());
    }

    #[test]
    fn sort_by_title_descending() {
        let mut pl = make(12);
        pl.sort_by_title(true);
        assert_eq!(pl.items[0].title, "T9");
        pl.reset();
        assert_eq!(pl.items[0].video_id, "v0");
    }
}
