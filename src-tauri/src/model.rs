//! ローカル管理のプレイリストモデル（API通信なし・クォータ消費なし）。

use rand::seq::SliceRandom;
use serde::Serialize;

#[derive(Clone, Serialize)]
pub struct PlaylistItem {
    pub playlist_item_id: String,
    pub video_id: String,
    pub title: String,
    pub original_position: usize,
    pub current_position: usize,
    pub thumbnail_url: String,
}

#[derive(Clone, Serialize)]
pub struct Playlist {
    pub playlist_id: String,
    pub title: String,
    pub items: Vec<PlaylistItem>,
}

impl Playlist {
    pub fn new(playlist_id: String, title: String, mut items: Vec<PlaylistItem>) -> Self {
        items.sort_by_key(|i| i.original_position);
        let mut p = Playlist { playlist_id, title, items };
        p.renumber();
        p
    }

    fn renumber(&mut self) {
        for (idx, item) in self.items.iter_mut().enumerate() {
            item.current_position = idx;
        }
    }

    pub fn sort_by_title(&mut self, descending: bool) {
        self.items.sort_by_cached_key(|i| i.title.to_lowercase());
        if descending {
            self.items.reverse();
        }
        self.renumber();
    }

    pub fn shuffle(&mut self) {
        self.items.shuffle(&mut rand::rng());
        self.renumber();
    }

    /// ドラッグ＆ドロップ用: from の要素を to へ移動。
    pub fn move_item(&mut self, from: usize, to: usize) -> Result<(), String> {
        let n = self.items.len();
        if from >= n || to >= n {
            return Err("index out of range".into());
        }
        let item = self.items.remove(from);
        self.items.insert(to, item);
        self.renumber();
        Ok(())
    }

    pub fn reset(&mut self) {
        self.items.sort_by_key(|i| i.original_position);
        self.renumber();
    }

    /// YouTube側へ書き戻すべき (playlist_item_id, 新position) を実行順に返す。
    ///
    /// playlistItems.update は1件ごとに他アイテムの位置をずらすため、
    /// サーバー側の並びをシミュレートし、すでに目標位置にある項目はスキップする。
    pub fn pending_updates(&self) -> Vec<(&PlaylistItem, usize)> {
        let mut server: Vec<&str> = {
            let mut v: Vec<&PlaylistItem> = self.items.iter().collect();
            v.sort_by_key(|i| i.original_position);
            v.into_iter().map(|i| i.playlist_item_id.as_str()).collect()
        };
        let mut updates = Vec::new();
        for (target, item) in self.items.iter().enumerate() {
            if server[target] == item.playlist_item_id {
                continue;
            }
            let cur = server.iter().position(|id| *id == item.playlist_item_id).unwrap();
            let id = server.remove(cur);
            server.insert(target, id);
            updates.push((item, target));
        }
        updates
    }

    /// 書き戻し済みの並び（playlist_item_id の順）を、サーバー側の新しい基準位置として記録する。
    /// 書き戻し中に別ウィンドウで並べ替えられても、サーバーと一致する基準を保てる。
    pub fn commit_order(&mut self, order: &[String]) {
        for item in &mut self.items {
            if let Some(i) = order.iter().position(|id| *id == item.playlist_item_id) {
                item.original_position = i;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(n: usize) -> Playlist {
        Playlist::new(
            "PL".into(),
            "T".into(),
            (0..n)
                .map(|k| PlaylistItem {
                    playlist_item_id: format!("i{k}"),
                    video_id: format!("v{k}"),
                    title: format!("T{k}"),
                    original_position: k,
                    current_position: k,
                    thumbnail_url: String::new(),
                })
                .collect(),
        )
    }

    fn apply(pl: &Playlist) -> Vec<String> {
        let mut server: Vec<String> = (0..pl.items.len()).map(|k| format!("i{k}")).collect();
        for (item, pos) in pl.pending_updates() {
            let cur = server.iter().position(|s| *s == item.playlist_item_id).unwrap();
            let id = server.remove(cur);
            server.insert(pos, id);
        }
        server
    }

    #[test]
    fn no_change_no_updates() {
        assert!(make(10).pending_updates().is_empty());
    }

    #[test]
    fn single_move_is_one_update() {
        let mut pl = make(10);
        pl.move_item(9, 0).unwrap();
        assert_eq!(pl.pending_updates().len(), 1);
    }

    #[test]
    fn random_orders_reach_target() {
        for n in 1..30 {
            for _ in 0..20 {
                let mut pl = make(n);
                pl.shuffle();
                let want: Vec<String> = pl.items.iter().map(|i| i.playlist_item_id.clone()).collect();
                assert_eq!(apply(&pl), want);
            }
        }
    }

    #[test]
    fn commit_order_keeps_edits_made_during_write_back() {
        let mut pl = make(4);
        pl.move_item(3, 0).unwrap();
        let sent: Vec<String> = pl.items.iter().map(|i| i.playlist_item_id.clone()).collect();
        pl.move_item(1, 2).unwrap(); // 書き戻し中の追加編集
        pl.commit_order(&sent);
        // サーバーは sent の並び。追加編集分だけが未反映として残る
        assert_eq!(pl.pending_updates().len(), 1);
    }

    #[test]
    fn sort_and_reset() {
        let mut pl = make(12);
        pl.sort_by_title(true);
        assert_eq!(pl.items[0].title, "T9");
        pl.reset();
        assert_eq!(pl.items[0].video_id, "v0");
    }
}
