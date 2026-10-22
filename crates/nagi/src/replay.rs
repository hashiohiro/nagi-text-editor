//! `--replay` のキー列に混ぜて書ける、画面を操作して確かめる手順（E2E の試験用）。
//!
//! - `<Click:場所>` `<DClick:場所>` `<RClick:場所>`: 左のクリック・ダブルクリック・右クリック。場所は `行,列`（セル）か、
//!   画面に描いた文字（いちばん上に描いたものの真ん中を押す。前後の空白を除いて同じ文字の所を先に、無ければ含む所）
//! - `<WaitFor:文字>` `<WaitGone:文字>`: その文字が出る・消えるまで待つ（10 秒まで。過ぎたら NG を記録して先へ）
//! - `<See:文字>` `<NotSee:文字>`: 今の画面にある・無いを確かめて記録する
//! - `<Wait:ミリ秒>` `<Shot:名前>`（画面の写しを BMP で）`<Dump>`（描いた文字を全部記録へ）`<Quit>`（閉じる）
//!
//! 記録は `NAGI_REPLAY_LOG` のファイル（無ければ標準エラー）へ 1 行ずつ（`OK …` / `NG …` / `INFO …`）。
//! 写しは `NAGI_REPLAY_OUT` のフォルダ（無ければ今のフォルダ）に `名前.bmp` で置く。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use nagi_vim::Key as VKey;

/// 文字が出るのを待つ上限
const WAIT_LIMIT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq)]
pub enum At {
    Cell(usize, usize),
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Key(VKey),
    Mouse { right: bool, clicks: u32, at: At },
    WaitFor { text: String, gone: bool, until: Option<Instant> },
    See { text: String, gone: bool },
    Wait(u64),
    Shot(String),
    Dump,
    Quit,
}

/// キー列を手順に分ける（`<名前:引数>` の形の手順と、それ以外のキー）
pub fn parse(s: &str, leader: &str) -> VecDeque<Step> {
    const NAMES: [&str; 10] = ["Click", "DClick", "RClick", "WaitFor", "WaitGone", "See", "NotSee", "Wait", "Shot", "Dump"];
    let mut out = VecDeque::new();
    let mut rest = s;
    let keys = |k: &str, out: &mut VecDeque<Step>| out.extend(nagi_vim::parse_keys(k, leader).into_iter().map(Step::Key));
    while let Some(i) = rest.find('<') {
        let tail = &rest[i + 1..];
        let name_end = tail.find([':', '>']).unwrap_or(tail.len());
        let name = &tail[..name_end];
        let step = if name == "Quit" && tail[name_end..].starts_with('>') {
            Some((Step::Quit, name_end + 1))
        } else if NAMES.contains(&name) {
            let body_start = name_end + usize::from(tail[name_end..].starts_with(':'));
            tail[body_start..].find('>').map(|e| (step_of(name, &tail[body_start..body_start + e]), body_start + e + 1))
        } else {
            None
        };
        match step {
            Some((st, len)) => {
                keys(&rest[..i], &mut out);
                out.push_back(st);
                rest = &tail[len..];
            }
            None => {
                // 手順ではない（`<CR>` などのキー）: そのままキーとして読む
                let end = tail.find('>').map_or(rest.len(), |e| i + 1 + e + 1);
                keys(&rest[..end], &mut out);
                rest = &rest[end..];
            }
        }
    }
    keys(rest, &mut out);
    out
}

fn step_of(name: &str, arg: &str) -> Step {
    let at = || match arg.split_once(',').and_then(|(r, c)| Some((r.trim().parse().ok()?, c.trim().parse().ok()?))) {
        Some((r, c)) => At::Cell(r, c),
        None => At::Text(arg.to_string()),
    };
    match name {
        "Click" => Step::Mouse { right: false, clicks: 1, at: at() },
        "DClick" => Step::Mouse { right: false, clicks: 2, at: at() },
        "RClick" => Step::Mouse { right: true, clicks: 1, at: at() },
        "WaitFor" => Step::WaitFor { text: arg.into(), gone: false, until: None },
        "WaitGone" => Step::WaitFor { text: arg.into(), gone: true, until: None },
        "See" => Step::See { text: arg.into(), gone: false },
        "NotSee" => Step::See { text: arg.into(), gone: true },
        "Wait" => Step::Wait(arg.trim().parse().unwrap_or(0)),
        "Shot" => Step::Shot(arg.into()),
        _ => Step::Dump,
    }
}

/// 描いた文字（行, 列, 文字）から `want` を探して、その真ん中のセル。後から描いたもの（上に重なるもの）を先に見る
pub fn find(texts: &[(usize, usize, String)], want: &str) -> Option<(usize, usize)> {
    let width = |s: &str| nagi_core::display_col(s, s.len(), false);
    let at = |(row, col, s): &(usize, usize, String), i: usize| (*row, col + width(&s[..i]) + width(want).saturating_sub(1) / 2);
    let exact = texts.iter().rev().find(|t| t.2.trim() == want).map(|t| at(t, t.2.find(want).unwrap_or(0)));
    exact.or_else(|| texts.iter().rev().find_map(|t| t.2.find(want).map(|i| at(t, i))))
}

/// 画面にあるか（1 つの文字の中に無くても、同じ行に続けて描いたものをつないで探す）
pub fn visible(texts: &[(usize, usize, String)], want: &str) -> bool {
    if texts.iter().any(|t| t.2.contains(want)) {
        return true;
    }
    let mut rows: std::collections::BTreeMap<usize, Vec<(usize, &str)>> = Default::default();
    for (r, c, s) in texts {
        rows.entry(*r).or_default().push((*c, s));
    }
    rows.values_mut().any(|v| {
        v.sort_by_key(|x| x.0);
        v.iter().map(|x| x.1).collect::<String>().contains(want)
    })
}

/// 記録へ 1 行（開けなければ少し待って開き直す。できたばかりのファイルを、ほかのプログラム（ウイルス対策の検査など）がつかんでいることがある）
pub fn log(line: &str) {
    use std::io::Write;
    let Some(p) = std::env::var_os("NAGI_REPLAY_LOG") else {
        eprintln!("{line}");
        return;
    };
    for _ in 0..20 {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
            if writeln!(f, "{line}").is_ok() {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    nagi_core::log::warn(&format!("記録に書けません: {line}"));
}

/// BGRA の画素を 32 bit の BMP（上の行から）で書く
pub fn write_bmp(path: &std::path::Path, w: u32, h: u32, px: &[u8]) -> std::io::Result<()> {
    let size = 54 + px.len() as u32;
    let mut b = Vec::with_capacity(size as usize);
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&size.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(w as i32).to_le_bytes());
    // 高さを負にすると上の行から
    b.extend_from_slice(&(-(h as i32)).to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(&[0u8; 24]);
    b.extend_from_slice(px);
    std::fs::write(path, b)
}

impl crate::Frame {
    /// 最後に描いたフレームの文字
    fn texts(&self) -> Vec<(usize, usize, String)> {
        self.ed.as_ref().map(|e| e.drawn_texts.borrow().clone()).unwrap_or_default()
    }

    /// 手順を 1 つ行う。待つ手順で、まだなら先頭に戻して true（少し後にもう一度）
    pub(crate) fn run_step(&mut self, step: Step, el: &winit::event_loop::ActiveEventLoop) -> bool {
        match step {
            Step::Key(k) => self.handle_keys(vec![k], el),
            Step::Mouse { right, clicks, at } => {
                let cell = match &at {
                    At::Cell(r, c) => Some((*r, *c)),
                    At::Text(t) => find(&self.texts(), t),
                };
                let (Some((row, col)), Some(f)) = (cell, &self.fonts) else {
                    log(&format!("NG click {at:?}: 画面にありません"));
                    return false;
                };
                let c = f.cell();
                self.mouse = ((col as f64 + 0.5) * c.w as f64, (row as f64 + 0.5) * c.h as f64);
                log(&format!("INFO {} {at:?} at {row},{col}", if right { "rclick" } else if clicks > 1 { "dclick" } else { "click" }));
                if right {
                    self.on_right_press();
                } else {
                    for _ in 0..clicks {
                        self.on_left(true, el);
                        self.on_left(false, el);
                    }
                }
                self.draw();
            }
            Step::WaitFor { text, gone, until } => {
                let until = until.unwrap_or_else(|| Instant::now() + WAIT_LIMIT);
                if visible(&self.texts(), &text) != gone {
                    log(&format!("OK {} {text}", if gone { "gone" } else { "shown" }));
                } else if Instant::now() < until {
                    self.replay.push_front(Step::WaitFor { text, gone, until: Some(until) });
                    return true;
                } else {
                    log(&format!("NG {} {text}: 10 秒待っても{}", if gone { "gone" } else { "shown" }, if gone { "消えません" } else { "出ません" }));
                }
            }
            Step::See { text, gone } => {
                let ok = visible(&self.texts(), &text) != gone;
                log(&format!("{} {} {text}", if ok { "OK" } else { "NG" }, if gone { "not-see" } else { "see" }));
            }
            Step::Wait(ms) => {
                self.replay_deadline = Some(Instant::now() + Duration::from_millis(ms));
                return true;
            }
            Step::Shot(name) => {
                if let Some(r) = &mut self.renderer {
                    r.capture = true;
                }
                self.draw();
                let dir = std::env::var_os("NAGI_REPLAY_OUT").map(std::path::PathBuf::from).unwrap_or_default();
                let path = dir.join(format!("{name}.bmp"));
                match self.renderer.as_mut().and_then(|r| r.captured.take()) {
                    Some((w, h, px)) => match write_bmp(&path, w, h, &px) {
                        Ok(()) => log(&format!("INFO shot {}", path.display())),
                        Err(e) => log(&format!("NG shot {}: {e}", path.display())),
                    },
                    None => log(&format!("NG shot {name}: 読み戻せません")),
                }
            }
            Step::Dump => {
                for (r, c, s) in self.texts() {
                    log(&format!("INFO text {r},{c} {s:?}"));
                }
            }
            Step::Quit => {
                // 窓が表に出ていたか（--headless なら出ていないはず）
                let visible = self.window.as_ref().and_then(|w| w.is_visible()).unwrap_or(false);
                log(&format!("INFO quit visible={visible}"));
                self.closed = true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_steps_between_keys() {
        let s = parse("jj<RClick:a.txt><WaitFor:変更を戻す…><Click:3,4><CR><Wait:200>y<Shot:after><Quit>", " ");
        let v: Vec<Step> = s.into_iter().collect();
        assert_eq!(
            v,
            vec![
                Step::Key(VKey::Char('j')),
                Step::Key(VKey::Char('j')),
                Step::Mouse { right: true, clicks: 1, at: At::Text("a.txt".into()) },
                Step::WaitFor { text: "変更を戻す…".into(), gone: false, until: None },
                Step::Mouse { right: false, clicks: 1, at: At::Cell(3, 4) },
                Step::Key(VKey::Enter),
                Step::Wait(200),
                Step::Key(VKey::Char('y')),
                Step::Shot("after".into()),
                Step::Quit,
            ]
        );
    }

    #[test]
    fn finds_text_on_top() {
        let t = vec![(2, 0, "  コミット".to_string()), (5, 10, " a.txt  src".to_string()), (2, 20, "  コミット".to_string())];
        // 後から描いた（上に重なる）ものの真ん中
        assert_eq!(find(&t, "コミット"), Some((2, 25)));
        assert_eq!(find(&t, "a.txt"), Some((5, 13)));
        assert_eq!(find(&t, "無い"), None);
        assert!(visible(&[(1, 0, " [x] ".into()), (1, 5, "M".into()), (1, 6, " a.txt".into())], "M a.txt"));
    }
}
