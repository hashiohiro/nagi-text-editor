//! IntelliJ の New UI の枠: 上の帯（メインツールバー）と、左右のツールウィンドウのアイコンの列（ストライプ）。
//!
//! 上の帯は 1 行目。左にプロジェクトとブランチ、右に実行の構成・▶・デバッグ・止める・どこでも検索・設定。
//! 左の列の上は左の欄（プロジェクト・コミット・実行）、下は下の欄（Git・検索・問題・ログ・ターミナル・デバッグ）、
//! 右の列は右の欄（データベース・Maven・プレビュー）のアイコン。押すとその欄を出す・隠す（IntelliJ と同じ）。
//! ほかの欄・窓は `area()`（枠の内側）に並べる。押した位置は `Editor::chrome_at` が当て、main.rs が動かす
//! （コマンドを動かすと Effect を返すものがあるため、layout の View の click ではなく main.rs で受け取る）。

use crate::editor::Editor;
use crate::panes::Rect;
use crate::result_grid::BottomKind;
use crate::right_panel::{RTABS, RightTab};
use crate::tree_panel::{LeftTab, TABS};

/// 上の帯の行の数
pub const TOOLBAR_ROWS: usize = 1;
/// 左右のアイコンの列の幅（列）
pub const STRIPE_W: usize = 3;
/// アイコン 1 つが使う行（アイコンの行と、間の 1 行）
const SLOT: usize = 2;

/// 枠の内側（狭すぎる画面では枠を出さない）
pub fn area((cols, rows): (usize, usize)) -> Rect {
    if cols < 2 * STRIPE_W + 30 || rows < TOOLBAR_ROWS + 6 {
        return Rect { x: 0, y: 0, w: cols, h: rows };
    }
    Rect { x: STRIPE_W, y: TOOLBAR_ROWS, w: cols - 2 * STRIPE_W, h: rows - TOOLBAR_ROWS }
}

/// ストライプのアイコンが出し入れする欄
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Left(LeftTab),
    Bottom(BottomKind),
    Right(RightTab),
}

/// 枠の押せる所
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    /// プロジェクト（最近のプロジェクト・開く）
    Projects,
    /// ブランチ（ブランチの一覧）
    Branches,
    /// 実行の構成（選ぶメニュー）
    RunMenu,
    /// コマンドを動かす（▶・デバッグ・止める・どこでも検索・設定）
    Cmd(&'static str),
    /// 欄を出す・隠す
    Tool(Tool),
}

/// 上の帯の部品（文字, 押したとき, 右寄せか）
struct Widget {
    text: String,
    hit: Hit,
    right: bool,
    /// 文字の前にアイコンを描く（その分 2 列）
    icon: Option<Icon>,
}

/// 上の帯の部品のアイコン（icons.rs の名前）
type Icon = &'static str;

fn width(s: &str) -> usize {
    nagi_core::display_col(s, s.len(), false)
}

/// 道具のアイコン（icons.rs の名前。IntelliJ のツールウィンドウのアイコン）
fn icon_of(t: Tool) -> &'static str {
    match t {
        Tool::Left(LeftTab::Project) => "tw_project",
        Tool::Left(LeftTab::Commit) => "tw_commit",
        Tool::Left(LeftTab::Run) => "tw_run",
        Tool::Bottom(BottomKind::Git) => "tw_vcs",
        Tool::Bottom(BottomKind::Grep) => "tw_find",
        Tool::Bottom(BottomKind::Problems) => "tw_problems",
        Tool::Bottom(BottomKind::Output) => "tw_messages",
        Tool::Bottom(BottomKind::Terminal) => "tw_terminal",
        Tool::Bottom(BottomKind::Debug) => "tw_debug",
        Tool::Bottom(BottomKind::Result) => "n_dataSchema",
        Tool::Right(RightTab::Database) => "n_dataSchema",
        Tool::Right(RightTab::Maven) => "tw_maven",
        Tool::Right(RightTab::Preview) => "a_preview",
    }
}

impl Editor {
    /// 枠を出しているか（狭い画面では出さない）
    pub fn chrome_shown(&self) -> bool {
        self.area().y > 0
    }

    /// 左の列のアイコン（行, 名前, 道具）。上は左の欄、下は下の欄（状態の行の上に下から積む）
    pub(crate) fn left_stripe(&self) -> Vec<(usize, &'static str, Tool)> {
        let a = self.area();
        let mut v: Vec<(usize, &'static str, Tool)> = TABS.iter().enumerate().map(|(i, (l, t))| (a.y + 1 + i * SLOT, *l, Tool::Left(*t))).collect();
        let bottom = self.bottom_tabs();
        let end = self.screen.1;
        for (i, (l, k)) in bottom.iter().enumerate() {
            let row = (end + i * SLOT).checked_sub(bottom.len() * SLOT);
            if let Some(row) = row.filter(|r| *r > v.last().map_or(a.y, |x| x.0 + 1)) {
                v.push((row, l, Tool::Bottom(*k)));
            }
        }
        v
    }

    /// 右の列のアイコン（行, 名前, 道具）
    pub(crate) fn right_stripe(&self) -> Vec<(usize, &'static str, Tool)> {
        let a = self.area();
        RTABS.iter().enumerate().map(|(i, (l, t))| (a.y + 1 + i * SLOT, *l, Tool::Right(*t))).collect()
    }

    /// その欄を出しているか（アイコンを明るく・地を付ける）
    pub(crate) fn tool_open(&self, t: Tool) -> bool {
        match t {
            Tool::Left(l) => self.left_visible() == Some(l),
            Tool::Bottom(k) => self.bottom_visible() && self.bottom_kind == k,
            Tool::Right(r) => self.right_visible() == Some(r),
        }
    }

    /// アイコンを押した: その欄を出す（出ていて移っていれば隠す）
    pub(crate) fn toggle_tool(&mut self, t: Tool) {
        match t {
            Tool::Left(LeftTab::Project) => self.toggle_tree(),
            Tool::Left(LeftTab::Commit) => self.toggle_commit(),
            Tool::Left(l) => self.toggle_left_tab(l),
            Tool::Bottom(k) => self.toggle_bottom_tab(k),
            Tool::Right(r) => self.toggle_right_tab(r),
        }
    }

    /// 上の帯の部品（左から順。右寄せのものは右から詰める）
    fn toolbar_widgets(&self) -> Vec<Widget> {
        let root = self.nav_root();
        let name = root.file_name().map_or_else(|| root.display().to_string(), |n| n.to_string_lossy().into_owned());
        let mut v = vec![Widget { text: format!("{name} ▾"), hit: Hit::Projects, right: false, icon: None }];
        if let Some(b) = self.git_branch_label() {
            v.push(Widget { text: format!("{b} ▾"), hit: Hit::Branches, right: false, icon: Some("g_vcs") });
        }
        let run = self.run_selected.clone().unwrap_or_else(|| "構成".into());
        v.push(Widget { text: format!("{run} ▾"), hit: Hit::RunMenu, right: true, icon: None });
        v.push(Widget { text: String::new(), hit: Hit::Cmd("Run"), right: true, icon: Some("r_run") });
        v.push(Widget { text: String::new(), hit: Hit::Cmd("Debug"), right: true, icon: Some("r_debug") });
        if self.run_selected.as_ref().is_some_and(|r| self.run_is_running(r)) {
            v.push(Widget { text: String::new(), hit: Hit::Cmd("RunStop"), right: true, icon: Some("r_stop") });
        }
        v.push(Widget { text: String::new(), hit: Hit::Cmd("SearchEverywhere"), right: true, icon: Some("g_search") });
        v.push(Widget { text: String::new(), hit: Hit::Cmd("Settings"), right: true, icon: Some("g_settings") });
        v
    }

    /// 上の帯の部品の位置（始めの列, 終わりの列, 部品）
    fn toolbar_layout(&self) -> Vec<(usize, usize, Widget)> {
        let cols = self.screen.0;
        let mut out = Vec::new();
        let (mut left, mut right) = (STRIPE_W, cols.saturating_sub(1));
        let ws = self.toolbar_widgets();
        let (l, r): (Vec<Widget>, Vec<Widget>) = ws.into_iter().partition(|w| !w.right);
        for w in l {
            let n = 2 + usize::from(w.icon.is_some()) * 2 + width(&w.text);
            out.push((left, left + n, w));
            left += n + 1;
        }
        let mut rs = Vec::new();
        for w in r.into_iter().rev() {
            let n = if w.text.is_empty() { 4 } else { 2 + usize::from(w.icon.is_some()) * 2 + width(&w.text) };
            if right < left + n {
                break;
            }
            rs.push((right - n, right, w));
            right -= n;
        }
        rs.reverse();
        out.extend(rs);
        out
    }

    /// 枠の (行, 列) の押せる所
    pub(crate) fn chrome_at(&self, row: usize, col: usize) -> Option<Hit> {
        if !self.chrome_shown() || row >= self.screen.1 {
            return None;
        }
        if row == 0 {
            return self.toolbar_layout().into_iter().find(|(a, b, _)| col >= *a && col < *b).map(|x| x.2.hit);
        }
        let a = self.area();
        let stripe = if col < a.x {
            self.left_stripe()
        } else if col >= a.x + a.w {
            self.right_stripe()
        } else {
            return None;
        };
        stripe.into_iter().find(|(r, ..)| *r == row).map(|(_, _, t)| Hit::Tool(t))
    }
}

/// 枠を描く（layout.rs の View。押した位置は main.rs が chrome_at で受け取る）
pub struct ChromeView;

impl crate::layout::View for ChromeView {
    fn rect(&self, ed: &Editor, s: crate::layout::Screen) -> Option<Rect> {
        ed.chrome_shown().then_some(Rect { x: 0, y: 0, w: s.cols, h: s.rows })
    }

    fn draw(&self, ed: &Editor, p: &mut crate::view::Painter, r: Rect, s: crate::layout::Screen) {
        let _ = r;
        draw::draw_chrome(p, ed, s);
    }
}

pub(crate) mod draw {
    use super::*;
    use crate::view::*;

    pub(crate) fn draw_chrome(p: &mut Painter, ed: &Editor, s: crate::layout::Screen) {
        let cell = p.fonts.cell();
        let a = ed.area();
        let (w, h) = (s.cols as f32 * cell.w, s.rows as f32 * cell.h);
        // 上の帯
        p.out.push(Instance::fill(0.0, 0.0, w, cell.h, PANEL_BG));
        p.out.push(Instance::fill(0.0, cell.h - 1.0, w, 1.0, SEPARATOR));
        // 左右の列
        let sw = a.x as f32 * cell.w;
        p.out.push(Instance::fill(0.0, cell.h, sw, h - cell.h, PANEL_BG));
        p.out.push(Instance::fill(sw - 1.0, cell.h, 1.0, h - cell.h, SEPARATOR));
        let rx = (a.x + a.w) as f32 * cell.w;
        p.out.push(Instance::fill(rx, cell.h, w - rx, h - cell.h, PANEL_BG));
        p.out.push(Instance::fill(rx, cell.h, 1.0, h - cell.h, SEPARATOR));
        let saved = p.cols;
        p.cols = s.cols;
        // アイコン（出している欄は地を付けて明るく。載せたら名前を出す）
        for (x0, items) in [(0usize, ed.left_stripe()), (a.x + a.w, ed.right_stripe())] {
            for (row, label, t) in items {
                let y = row as f32 * cell.h;
                let open = ed.tool_open(t);
                // IntelliJ と同じく、出している欄のアイコンは角の丸い地の上に
                let px = crate::icons::large(cell.h);
                let pad = (cell.h * 0.25).round();
                let bx = x0 as f32 * cell.w + ((STRIPE_W as f32 * cell.w - px) / 2.0).round();
                let by = crate::icons::centered(y, cell.h, px);
                if open {
                    rounded(p, bx - pad, by - pad, px + 2.0 * pad, px + 2.0 * pad, LIST_SEL_BG);
                }
                crate::icons::draw(p, icon_of(t), bx, by, px, None);
                p.tab_hits.push((row, x0, x0 + STRIPE_W, label));
            }
        }
        // 上の帯の部品
        for (c0, c1, wd) in ed.toolbar_layout() {
            let hover = ed.hover_chrome == Some(wd.hit);
            if hover {
                p.out.push(Instance::fill(c0 as f32 * cell.w, 2.0, (c1 - c0) as f32 * cell.w, cell.h - 5.0, RAISED_BG));
            }
            let mut c = c0 + 1;
            if let Some(name) = wd.icon {
                let px = crate::icons::small(cell.h);
                let cx = if wd.text.is_empty() { (c0 + c1) as f32 / 2.0 * cell.w - px / 2.0 } else { c as f32 * cell.w };
                crate::icons::draw(p, name, cx, crate::icons::centered(0.0, cell.h, px), px, None);
                c += 2;
            }
            if !wd.text.is_empty() {
                let fg = if wd.hit == Hit::Projects { FG_BRIGHT } else { FG };
                p.text(&wd.text, c, 0.0, fg);
            }
            let tip = match wd.hit {
                Hit::Cmd("Run") => Some("実行（Shift+F10）"),
                Hit::Cmd("Debug") => Some("デバッグ（Shift+F9）"),
                Hit::Cmd("RunStop") => Some("止める（Ctrl+F2）"),
                Hit::Cmd("SearchEverywhere") => Some("どこでも検索（Shift 2 回）"),
                Hit::Cmd("Settings") => Some("設定（Ctrl+Alt+S）"),
                _ => None,
            };
            if let Some(t) = tip {
                p.tab_hits.push((0, c0, c1, t));
            }
        }
        p.cols = saved;
    }

    /// 角の丸い四角（角の 1 px を欠く）
    pub(crate) fn rounded(p: &mut Painter, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        let r = 3.0;
        p.out.push(Instance::fill(x + r, y, w - 2.0 * r, h, c));
        p.out.push(Instance::fill(x, y + r, r, h - 2.0 * r, c));
        p.out.push(Instance::fill(x + w - r, y + r, r, h - 2.0 * r, c));
        p.out.push(Instance::fill(x + 1.0, y + 1.0, r - 1.0, r - 1.0, c));
        p.out.push(Instance::fill(x + w - r, y + 1.0, r - 1.0, r - 1.0, c));
        p.out.push(Instance::fill(x + 1.0, y + h - r, r - 1.0, r - 1.0, c));
        p.out.push(Instance::fill(x + w - r, y + h - r, r - 1.0, r - 1.0, c));
    }
}
