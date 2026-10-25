//! YouTube Data API の1日のクォータ（10,000ユニット）の概算管理と、別日に回す変更の保存。
//!
//! クォータは太平洋時間の0時（夏時間あり）にリセットされる。使用量はこのアプリが消費した分だけの推定で、
//! 同じプロジェクトを使う他のツールの消費は含まない（実際に上限へ達したときは API のエラーで検知する）。

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::Playlist;
use crate::youtube::PlaylistSummary;

pub const DAILY_LIMIT: u32 = 10_000;
/// 書き込み系の操作（insert / update / delete）1回のコスト。
pub const OP_COST: u32 = 50;

static IO: Mutex<()> = Mutex::new(());

pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

// ---- 太平洋時間の日付計算（chrono なし） ----

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn year_of_days(days: i64) -> i64 {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    if mp >= 10 { y + 1 } else { y }
}

/// 指定した月の「第 n 日曜」の通算日。
fn nth_sunday(y: i64, m: i64, n: i64) -> i64 {
    let first = days_from_civil(y, m, 1);
    let wd = (first + 4).rem_euclid(7); // 0 = 日曜（1970-01-01 は木曜）
    first + (7 - wd) % 7 + 7 * (n - 1)
}

/// 太平洋時間の UTC からのオフセット（秒）。米国の夏時間: 3月第2日曜 2:00 〜 11月第1日曜 2:00。
fn pacific_offset(utc: i64) -> i64 {
    let y = year_of_days(utc.div_euclid(86400));
    let start = nth_sunday(y, 3, 2) * 86400 + 10 * 3600; // 2:00 PST = 10:00 UTC
    let end = nth_sunday(y, 11, 1) * 86400 + 9 * 3600; // 2:00 PDT = 09:00 UTC
    if utc >= start && utc < end { -7 * 3600 } else { -8 * 3600 }
}

/// 太平洋時間での通算日（クォータの「1日」の単位）。
fn pacific_day(utc: i64) -> i64 {
    (utc + pacific_offset(utc)).div_euclid(86400)
}

/// 次のクォータリセット（太平洋時間の0時）までの秒数。
pub fn secs_until_reset(utc: i64) -> i64 {
    let local = utc + pacific_offset(utc);
    86400 - local.rem_euclid(86400)
}

// ---- 使用量の記録 ----

#[derive(Serialize, Deserialize, Default)]
struct Usage {
    day: i64,
    used: u32,
}

fn usage_path() -> PathBuf {
    crate::youtube::storage_dir().join("quota.json")
}

fn load_usage(now: i64) -> Usage {
    let u: Usage = std::fs::read_to_string(usage_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if u.day == pacific_day(now) { u } else { Usage { day: pacific_day(now), used: 0 } }
}

fn store_usage(u: &Usage) {
    if let Ok(t) = serde_json::to_string(u) {
        let _ = std::fs::write(usage_path(), t);
    }
}

/// 今日（太平洋時間）にこのアプリが消費した推定ユニット数。
pub fn used_today() -> u32 {
    let _io = IO.lock().unwrap();
    load_usage(now_secs()).used
}

pub fn remaining() -> u32 {
    DAILY_LIMIT.saturating_sub(used_today())
}

pub fn add(units: u32) {
    let _io = IO.lock().unwrap();
    let mut u = load_usage(now_secs());
    u.used = u.used.saturating_add(units);
    store_usage(&u);
}

/// API が上限超過を返したとき、今日の残りを 0 にする。
pub fn mark_exhausted() {
    let _io = IO.lock().unwrap();
    let mut u = load_usage(now_secs());
    u.used = DAILY_LIMIT;
    store_usage(&u);
}

/// API のエラーメッセージがクォータ超過か。
pub fn is_quota_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("quota") || m.contains("ratelimitexceeded") || m.contains("dailylimitexceeded")
}

/// 残りのユニットで実行できる操作数。
pub fn affordable_ops() -> usize {
    (remaining() / OP_COST) as usize
}

/// ops 件を実行するのに必要な日数（今日を含む）。
pub fn days_needed(ops: usize) -> u32 {
    let cost = ops as u32 * OP_COST;
    let rem = remaining();
    if cost <= rem { 1 } else { 1 + (cost - rem).div_ceil(DAILY_LIMIT) }
}

// ---- 別日に回す変更の保存 ----

#[derive(Serialize, Deserialize, Default)]
pub struct Deferred {
    pub playlists: Vec<Playlist>,
    pub summaries: Vec<PlaylistSummary>,
    pub next_local: u32,
    pub next_item: u32,
}

fn deferred_path() -> PathBuf {
    crate::youtube::storage_dir().join("deferred.json")
}

/// 未反映の変更を保存する。変更が無ければファイルを消す。
pub fn save_deferred(d: &Deferred) {
    let _io = IO.lock().unwrap();
    if d.playlists.is_empty() {
        let _ = std::fs::remove_file(deferred_path());
        return;
    }
    if let Ok(t) = serde_json::to_string(d) {
        let p = deferred_path();
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

fn cache_path() -> PathBuf {
    crate::youtube::storage_dir().join("cache.json")
}

/// 前回取得した内容（一覧と取得済みのリスト）。毎回取得し直さずに表示するために保存する。
pub fn save_cache_json(json: &str) {
    let _io = IO.lock().unwrap();
    let p = cache_path();
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

pub fn load_cache() -> Option<Deferred> {
    let _io = IO.lock().unwrap();
    serde_json::from_str(&std::fs::read_to_string(cache_path()).ok()?).ok()
}

pub fn load_deferred() -> Option<Deferred> {
    let _io = IO.lock().unwrap();
    let d: Deferred = serde_json::from_str(&std::fs::read_to_string(deferred_path()).ok()?).ok()?;
    (!d.playlists.is_empty()).then_some(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(y: i64, m: i64, d: i64, h: i64) -> i64 {
        days_from_civil(y, m, d) * 86400 + h * 3600
    }

    #[test]
    fn pacific_offset_follows_dst() {
        assert_eq!(pacific_offset(ts(2026, 1, 15, 12)), -8 * 3600);
        assert_eq!(pacific_offset(ts(2026, 7, 15, 12)), -7 * 3600);
        // 2026: 夏時間は 3/8 開始、11/1 終了
        assert_eq!(pacific_offset(ts(2026, 3, 8, 9)), -8 * 3600);
        assert_eq!(pacific_offset(ts(2026, 3, 8, 11)), -7 * 3600);
        assert_eq!(pacific_offset(ts(2026, 11, 1, 8)), -7 * 3600);
        assert_eq!(pacific_offset(ts(2026, 11, 1, 10)), -8 * 3600);
    }

    #[test]
    fn reset_is_pacific_midnight() {
        // 夏時間中の 0:00 PDT = 07:00 UTC
        assert_eq!(secs_until_reset(ts(2026, 7, 15, 6)), 3600);
        // 標準時間中の 0:00 PST = 08:00 UTC
        assert_eq!(secs_until_reset(ts(2026, 1, 15, 7)), 3600);
        assert_ne!(pacific_day(ts(2026, 7, 15, 6)), pacific_day(ts(2026, 7, 15, 8)));
    }

    #[test]
    fn days_needed_counts_today() {
        // 使用量ファイルに依存しないよう、境界だけを確認する
        assert!(days_needed(0) >= 1);
        assert!(days_needed(10_000) > 1);
    }

    #[test]
    fn quota_error_detection() {
        assert!(is_quota_error("YouTube API 403 Forbidden: The request cannot be completed because you have exceeded your quota."));
        assert!(!is_quota_error("YouTube API 404 Not Found: Video not found"));
    }
}
