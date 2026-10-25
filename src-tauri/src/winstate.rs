//! ウィンドウの位置・大きさ・最大化を INI ファイル（window.ini）に保存し、次回起動時に復元する。
//!
//! 保存先は認証情報（token.json）と同じフォルダ。ウィンドウごとにセクション（[main]、[editor-…]）を持つ。

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{Manager, PhysicalPosition, PhysicalSize, Window, WindowEvent, WebviewWindow};

type Ini = BTreeMap<String, BTreeMap<String, String>>;

static IO: Mutex<()> = Mutex::new(());
static LAST_SAVE: Mutex<Option<Instant>> = Mutex::new(None);
/// 復元が済んだウィンドウ。生成直後の初期値の Resized/Moved で、保存済みの値を上書きしないために使う。
static READY: Mutex<Option<HashSet<String>>> = Mutex::new(None);

const MIN_W: u32 = 400;
const MIN_H: u32 = 300;

fn path() -> PathBuf {
    crate::youtube::storage_dir().join("window.ini")
}

fn parse(text: &str) -> Ini {
    let mut ini = Ini::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_string();
            ini.entry(section.clone()).or_default();
        } else if let Some((k, v)) = line.split_once('=') {
            ini.entry(section.clone()).or_default().insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    ini
}

fn render(ini: &Ini) -> String {
    let mut out = String::from("; YouTube Playlist Player: ウィンドウの位置と大きさ（自動で保存・復元されます）\n");
    for (section, keys) in ini {
        out.push_str(&format!("\n[{section}]\n"));
        for (k, v) in keys {
            out.push_str(&format!("{k}={v}\n"));
        }
    }
    out
}

fn read() -> Ini {
    std::fs::read_to_string(path()).map(|t| parse(&t)).unwrap_or_default()
}

fn write(ini: &Ini) {
    let p = path();
    let tmp = p.with_extension("ini.tmp");
    if std::fs::write(&tmp, render(ini)).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

/// 保存済みの左上の点が、いずれかのモニター内に収まっているか（接続画面の変更で画面外に出るのを防ぐ）。
/// monitors: (x, y, 幅, 高さ)
fn visible_on(monitors: &[(i32, i32, u32, u32)], x: i32, y: i32) -> bool {
    let (px, py) = (x + 50, y + 20);
    monitors
        .iter()
        .any(|&(mx, my, mw, mh)| px >= mx && px < mx + mw as i32 && py >= my && py < my + mh as i32)
}

/// 現在のウィンドウ状態を保存する。最小化中は何もしない。最大化中は、元の位置・大きさを保ったまま最大化だけ記録する。
pub fn save(window: &WebviewWindow) {
    let (Ok(maximized), Ok(minimized)) = (window.is_maximized(), window.is_minimized()) else { return };
    if minimized {
        return;
    }
    let _io = IO.lock().unwrap();
    let mut ini = read();
    let sec = ini.entry(window.label().to_string()).or_default();
    if maximized {
        sec.insert("maximized".into(), "true".into());
    } else if let (Ok(p), Ok(s)) = (window.outer_position(), window.inner_size()) {
        sec.insert("x".into(), p.x.to_string());
        sec.insert("y".into(), p.y.to_string());
        sec.insert("width".into(), s.width.to_string());
        sec.insert("height".into(), s.height.to_string());
        sec.insert("maximized".into(), "false".into());
    }
    write(&ini);
}

/// 保存済みの位置・大きさ・最大化を適用する。済んだら、そのウィンドウの保存を有効にする。
pub fn restore(window: &WebviewWindow) {
    {
        let _io = IO.lock().unwrap();
        if let Some(sec) = read().get(window.label()) {
            let num = |k: &str| sec.get(k).and_then(|v| v.parse::<i64>().ok());
            if let (Some(w), Some(h)) = (num("width"), num("height")) {
                if w >= MIN_W as i64 && h >= MIN_H as i64 {
                    let _ = window.set_size(PhysicalSize::new(w as u32, h as u32));
                }
            }
            if let (Some(x), Some(y)) = (num("x"), num("y")) {
                let monitors: Vec<_> = window
                    .available_monitors()
                    .unwrap_or_default()
                    .iter()
                    .map(|m| (m.position().x, m.position().y, m.size().width, m.size().height))
                    .collect();
                if visible_on(&monitors, x as i32, y as i32) {
                    let _ = window.set_position(PhysicalPosition::new(x as i32, y as i32));
                }
            }
            if sec.get("maximized").map(String::as_str) == Some("true") {
                let _ = window.maximize();
            }
        }
    }
    READY.lock().unwrap().get_or_insert_with(HashSet::new).insert(window.label().to_string());
}

fn is_ready(label: &str) -> bool {
    READY.lock().unwrap().as_ref().is_some_and(|s| s.contains(label))
}

/// Builder::on_window_event に渡す。移動・サイズ変更は間引いて保存し、閉じるときは必ず保存する。
pub fn on_event(window: &Window, event: &WindowEvent) {
    let app = window.app_handle();
    let Some(ww) = app.get_webview_window(window.label()) else { return };
    match event {
        WindowEvent::Resized(_) | WindowEvent::Moved(_) => {
            if !is_ready(ww.label()) {
                return;
            }
            let mut last = LAST_SAVE.lock().unwrap();
            if last.is_some_and(|t| t.elapsed() < Duration::from_millis(800)) {
                return;
            }
            *last = Some(Instant::now());
            drop(last);
            save(&ww);
        }
        WindowEvent::CloseRequested { .. } => {
            if !is_ready(ww.label()) {
                return;
            }
            if ww.label() == "main" {
                // メインを閉じるとアプリが終了するので、開いている並べ替えウィンドウもまとめて保存する
                for w in app.webview_windows().values() {
                    if is_ready(w.label()) {
                        save(w);
                    }
                }
            } else {
                save(&ww);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_roundtrip() {
        let mut ini = Ini::new();
        ini.entry("main".into()).or_default().insert("width".into(), "1200".into());
        ini.entry("main".into()).or_default().insert("x".into(), "-8".into());
        ini.entry("editor-PL1".into()).or_default().insert("maximized".into(), "true".into());
        let parsed = parse(&render(&ini));
        assert_eq!(parsed, ini);
    }

    #[test]
    fn parse_ignores_comments_and_blank_lines() {
        let ini = parse("; c\n# c2\n\n[main]\n width = 800 \nbad line\n");
        assert_eq!(ini["main"]["width"], "800");
        assert_eq!(ini["main"].len(), 1);
    }

    #[test]
    fn offscreen_position_is_rejected() {
        let monitors = [(0, 0, 1920, 1080), (1920, 0, 1280, 1024)];
        assert!(visible_on(&monitors, 100, 100));
        assert!(visible_on(&monitors, 2000, 50));
        assert!(!visible_on(&monitors, 5000, 100));   // 外したモニター
        assert!(!visible_on(&monitors, -32000, -32000)); // 最小化時の座標
    }
}
