//! git の画面の 5 段目: 履歴（グラフ付きのログ。IntelliJ の Git ログと同じ形）。
//!
//! 窓の領域全体に、左にブランチ、中央にグラフ付きのコミット、右に選んだコミットのメッセージと変えたファイルを並べる。
//! ログ・ブランチ・コミットの中身は裏で git から取る。ファイルの上で Enter を押すと、1 つ前の版との差分を開く。


use nagi_vim::Key;

use crate::editor::{Editor, State};

#[derive(Clone, Debug, Default)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub refs: String,
    pub subject: String,
    pub author: String,
    pub date: String,
}

#[derive(Clone, Debug)]
pub struct LogRow {
    pub graph: String,
    pub commit: Option<Commit>,
}

impl LogRow {
    /// 真ん中の列で横に送る部分（グラフ・ref・題名）の表示の幅（描き方と同じ並び）
    pub fn text_width(&self) -> usize {
        let w = |s: &str| nagi_core::display_col(s, s.len(), false);
        let graph = w(&pretty_graph(&self.graph));
        match &self.commit {
            Some(c) => graph + 1 + if c.refs.is_empty() { 0 } else { w(&c.refs) + 2 } + w(&c.subject),
            None => graph,
        }
    }
}

/// 履歴（幅 `w`）の左のブランチの列と右の中身の列の幅（ドラッグで変えていればその幅。真ん中に 20 列は残す）
pub fn log_cols(w: usize, branch_w: Option<usize>, detail_w: Option<usize>) -> (usize, usize) {
    let bmin = 6.min(w);
    let bw = branch_w.unwrap_or(20.min(w / 6)).clamp(bmin, (w / 2).max(bmin));
    let dmin = 10.min(w);
    let dw = detail_w.unwrap_or(42.min(w / 3)).clamp(dmin, w.saturating_sub(bw + 2 + 20).max(dmin));
    (bw, dw)
}

/// 作者・日時・ハッシュの列の、中身に合わせた幅（作者と日時は長すぎるものに引きずられないよう 16 列まで）
fn auto_cols(rows: &[LogRow]) -> (usize, usize, usize) {
    let w = |s: &str| nagi_core::display_col(s, s.len(), false);
    let cs = rows.iter().filter_map(|r| r.commit.as_ref());
    let (mut a, mut d, mut h) = (4, 4, 7);
    for c in cs {
        a = a.max(w(&c.author).min(16));
        d = d.max(w(&c.date).min(16));
        h = h.max(c.short.len());
    }
    (a, d, h)
}

/// 履歴の真ん中の列で、横に送る部分の見える幅（右の作者・日時・ハッシュの列 `(aw, tw, hw)` の分を除く）
pub fn log_text_w(w: usize, (bw, dw): (usize, usize), (aw, tw, hw): (usize, usize, usize)) -> usize {
    w.saturating_sub(bw + dw + 2).saturating_sub(aw + tw + hw + 4).max(1)
}

/// 右の中身の列の 1 行
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DetailLine {
    /// ハッシュ・作者・日時・親（薄い字）
    Info(String),
    /// メッセージ（true は 1 行目。題名）
    Message(String, bool),
    Gap,
    /// 「変えたファイル（n）」
    FilesHead(usize),
    /// 変えたファイル（files の番号）
    File(usize),
}

/// 右の中身の列の行（描き方と押した位置の割り付けに使う）。ファイルのほかは幅 `w` で折り返す（メッセージは全部出す）
pub fn detail_lines(d: &Detail, w: usize) -> Vec<DetailLine> {
    let short = |h: &str| h[..7.min(h.len())].to_string();
    let mut out = Vec::new();
    let info = |out: &mut Vec<DetailLine>, s: String| out.extend(wrap(&s, w).into_iter().map(DetailLine::Info));
    if let Some(b) = &d.base {
        info(&mut out, format!("比較  {} → {}", short(b), short(&d.hash)));
        info(&mut out, "左が古い方、右が新しい方".into());
    } else {
        info(&mut out, format!("{}   {}   {}", short(&d.hash), d.author, d.date));
        info(&mut out, format!("親 {}", d.parents.split(' ').map(short).collect::<Vec<_>>().join(" ")));
    }
    out.push(DetailLine::Gap);
    for (k, m) in d.message.lines().enumerate() {
        out.extend(wrap(m, w).into_iter().map(|l| DetailLine::Message(l, k == 0)));
    }
    out.push(DetailLine::Gap);
    out.push(DetailLine::FilesHead(d.files.len()));
    out.extend((0..d.files.len()).map(DetailLine::File));
    out
}

/// 変えたファイルの行の字（状態の文字・名前・フォルダ。IntelliJ と同じく名前を先に、フォルダは後ろに薄く）
pub fn file_line(st: char, path: &str) -> (String, String) {
    let (dir, name) = path.rsplit_once('/').map_or(("", path), |(d, n)| (d, n));
    (format!("{st}  {name}"), if dir.is_empty() { String::new() } else { format!("  {dir}") })
}

/// 表示の幅 `w` で折り返す（空白があればそこで切る。全角は 2 セル）
pub fn wrap(s: &str, w: usize) -> Vec<String> {
    let cw = |t: &str| nagi_core::display_col(t, t.len(), false);
    if w == 0 || cw(s) <= w {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    // 今の行の幅は足し上げて持つ（1 文字ごとに行全体を数え直さない）
    let mut cur_w = 0;
    for ch in s.chars() {
        let mut b = [0u8; 4];
        let chw = cw(ch.encode_utf8(&mut b));
        if !cur.is_empty() && cur_w + chw > w {
            match cur.rfind(' ').filter(|&i| i > 0 && ch != ' ') {
                Some(i) => {
                    let rest = cur[i + 1..].to_string();
                    cur.truncate(i);
                    cur_w = cw(&rest);
                    out.push(std::mem::replace(&mut cur, rest));
                }
                None => {
                    out.push(std::mem::take(&mut cur));
                    cur_w = 0;
                }
            }
            if ch == ' ' {
                continue;
            }
        }
        cur.push(ch);
        cur_w += chw;
    }
    out.push(cur);
    out
}

/// 履歴の割り付け（`LogView::ui`）に置くもの。描き方と押した位置の両方がこれを使う（ADR 0018 の §1）
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogHit {
    /// 0 行目の絞り込み
    Filter,
    /// 0 行目のブランチ・ユーザー・日付の絞り込み（IntelliJ の VCS ログの上の帯と同じ）
    Chip(LogChip),
    /// 左の列の 1 行（区分・フォルダ・ブランチ）
    Branch(BranchRow),
    BranchArea,
    /// 真ん中のコミットの行（visible の番号）と、グラフだけの行
    Commit(usize),
    Graph(usize),
    CommitArea,
    /// 右の中身の 1 行
    Detail(DetailLine),
    DetailArea,
}

/// 履歴のキーで、部品の外に及ぶこと（Editor が行う。ADR 0018 の §5）
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogAction {
    None,
    /// 本文へ戻る（下の欄に収めたまま）
    ToText,
    /// 下の欄を隠す
    HideBottom,
    /// 履歴を閉じる
    Close,
    /// 選んだファイルを右の列の見える範囲へ
    FitDetail,
    /// 選んだファイルの差分を開く
    OpenFileDiff,
    /// このコミットをチェックアウトするか確かめる
    AskCheckout(String),
    /// 選んだコミットをチェリーピック
    CherryPick,
    /// `:Gbranch` の行に名前を打たせる（起点のハッシュ）
    PromptBranch(String),
    /// ハッシュをコピー
    Copy,
}

/// 履歴のアクション（ui_action.rs。ADR 0019）。対象は選んでいるコミット（複数選んでいればコピー・チェリーピックはそれら）
pub static ACTIONS: [crate::ui_action::UiAction; 8] = {
    use crate::ui_action::{Scope::Log, UiAction};
    fn open(e: &Editor) -> bool {
        e.panels.log.is_some()
    }
    fn commit(e: &Editor) -> bool {
        e.panels.log.as_ref().is_some_and(|l| l.current().is_some())
    }
    fn docked(e: &Editor) -> bool {
        e.panels.log.as_ref().is_some_and(|l| l.docked)
    }
    /// 選んでいるコミットの (ハッシュ, 短いハッシュ)
    fn cur(e: &Editor) -> Option<(String, String)> {
        e.panels.log.as_ref().and_then(|l| l.current()).map(|c| (c.hash.clone(), c.short.clone()))
    }
    [
        UiAction { id: "log.copyHash", label: "リビジョン番号のコピー", scope: Log, enabled: commit, run: |e| e.log_act(LogAction::Copy) },
        UiAction { id: "log.copyHashSubject", label: "ハッシュと題名をコピー", scope: Log, enabled: commit, run: |e| e.log_copy_picked(true) },
        UiAction { id: "log.cherryPick", label: "cherry-pick", scope: Log, enabled: commit, run: |e| e.log_act(LogAction::CherryPick) },
        UiAction { id: "log.checkout", label: "リビジョンのチェックアウト", scope: Log, enabled: commit, run: |e| {
            if let Some((h, _)) = cur(e) {
                e.log_act(LogAction::AskCheckout(h));
            }
        } },
        UiAction { id: "log.newBranch", label: "新規ブランチ…", scope: Log, enabled: commit, run: |e| {
            if let Some((_, s)) = cur(e) {
                e.log_act(LogAction::PromptBranch(s));
            }
        } },
        UiAction { id: "log.filter", label: "フィルター", scope: Log, enabled: open, run: |e| {
            if let Some(l) = &mut e.panels.log {
                l.filter = Some(crate::vim_field::VimField::new(&l.filter_text, false));
            }
        } },
        UiAction { id: "log.hide", label: "非表示", scope: Log, enabled: docked, run: |e| e.log_act(LogAction::HideBottom) },
        UiAction { id: "log.close", label: "履歴を閉じる", scope: Log, enabled: open, run: |e| e.log_act(LogAction::Close) },
    ]
};

/// ブランチの列の 1 行（nagi-ui の木の行）: 区分（depth 0）か、名前の / で分けたフォルダ（`key` は区分からの道筋。ローカル/feature）か、
/// ブランチ（Leaf の `i` は branches の番号）
pub type BranchRow = nagi_ui::PathRow;

#[derive(Clone, Debug, Default)]
pub struct Detail {
    pub hash: String,
    pub author: String,
    pub date: String,
    pub parents: String,
    pub message: String,
    /// (状態の文字, パス)
    pub files: Vec<(char, String)>,
    /// 2 つのコミットを比べているときの古い方（hash が新しい方。files はその間で変わったファイル）
    pub base: Option<String>,
}

/// 確かめてから行う操作（確認の窓で「はい」なら log_do）
#[derive(Clone, Debug)]
pub enum Confirm {
    Checkout(String),
    CherryPick(String),
    Revert(String),
    /// 複数のチェリーピック（古い順）
    CherryPickMany(Vec<String>),
    /// 複数の取り消し（新しい順）
    RevertMany(Vec<String>),
    /// HEAD のコミットを取り消して、変更は作業ツリーに残す（IntelliJ の Undo Commit）
    Undo(String),
    /// 親にまとめる（true ならメッセージをつなぐスカッシュ、false ならフィックスアップ）
    Squash(String, bool),
    Drop(String),
    /// このコミットまでリセット（--soft・--mixed・--hard・--keep）
    Reset(String, &'static str),
    /// 今のブランチをこの上へ載せ直す
    Rebase(String),
    /// ここまで push（ハッシュ, 上流のブランチ）
    PushTo(String, String),
    /// ブランチを消す（名前, 強制）
    DeleteBranch(String, bool),
    /// このブランチをチェックアウトして、さっきまでのブランチの上へ載せ直す
    CheckoutRebase(String, String),
}

type Rx<T> = Option<nagi_core::task::Task<nagi_core::Result<T>>>;

/// 確かめた操作のうち、手元で済むもの（成功したら知らせる文）
fn log_op(root: &std::path::Path, c: &Confirm) -> nagi_core::Result<String> {
    use nagi_workers::git;
    match c {
        Confirm::Checkout(h) => git::git(root, &["checkout", "-q", h]).map(|_| "チェックアウトしました".into()),
        Confirm::CherryPick(h) => git::cherry_pick(root, h).map(|_| "チェリーピックしました".into()),
        Confirm::Revert(h) => git::revert(root, h).map(|_| "取り消すコミットを足しました".into()),
        Confirm::CherryPickMany(hs) => git::cherry_pick_many(root, hs).map(|_| format!("{} 件をチェリーピックしました", hs.len())),
        Confirm::RevertMany(hs) => git::revert_many(root, hs).map(|_| format!("{} 件を取り消すコミットを足しました", hs.len())),
        Confirm::Undo(_) => git::git(root, &["reset", "--soft", "HEAD~1"]).map(|_| "コミットを取り消しました（変更は残っています）".into()),
        Confirm::Squash(h, keep) => git::squash_into_parent(root, h, *keep).map(|_| "1 つ前のコミットにまとめました".into()),
        Confirm::Drop(h) => git::drop_commit(root, h).map(|_| "コミットを消しました".into()),
        Confirm::Reset(h, m) => git::git(root, &["reset", m, h]).map(|_| format!("リセットしました（{m}）")),
        Confirm::Rebase(h) => git::rebase_onto(root, h).map(|_| "リベースしました".into()),
        Confirm::CheckoutRebase(b, onto) => git::git(root, &["checkout", "-q", b]).and_then(|_| git::rebase_onto(root, onto)).map(|_| format!("{b} を {onto} の上へリベースしました")),
        Confirm::PushTo(..) | Confirm::DeleteBranch(..) => unreachable!("log_do で扱う"),
    }
}

/// 0 行目の絞り込みの札
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogChip {
    Branch,
    User,
    Date,
}

/// 日付で絞り込む期間（IntelliJ の VCS ログと同じ選び方）
pub const LOG_PERIODS: [(&str, &str); 4] = [("過去 24 時間", "1.day.ago"), ("過去 7 日間", "7.days.ago"), ("過去 30 日間", "30.days.ago"), ("過去 1 年間", "1.year.ago")];

/// 一度に読むコミットの数（見えている所が終わりに近づいたら倍にして読み直す）
pub const LOG_PAGE: usize = 1000;
/// 読み直して増やす上限（注釈から開いたコミットを探すときも、ここまで）
const LOG_MAX: usize = 64_000;

/// ブランチ・ユーザー・日付の絞り込み（git log に渡す）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogQuery {
    pub branch: Option<String>,
    pub user: Option<String>,
    /// LOG_PERIODS の番号
    pub period: Option<usize>,
}

/// git log の引数（`path` はファイルの履歴、`range` は比べる範囲かブランチ）
pub fn log_args(path: Option<&str>, range: Option<&str>, q: &LogQuery, limit: usize) -> Vec<String> {
    let mut a: Vec<String> = vec!["log".into()];
    if path.is_some() {
        // ファイルの履歴は名前の変更をたどる（--follow はグラフと一緒に使えない）
        a.push("--follow".into());
    } else {
        a.extend(["--graph".into(), "--date-order".into()]);
    }
    a.extend(["-n".into(), limit.to_string(), "--format=\x1f%h\x1f%D\x1f%s\x1f%an\x1f%at\x1f%H".into()]);
    if let Some(u) = &q.user {
        a.push(format!("--author={}", regex::escape(u)));
    }
    if let Some(&(_, since)) = q.period.and_then(|i| LOG_PERIODS.get(i)) {
        a.push(format!("--since={since}"));
    }
    match (q.branch.as_deref().or(range), path) {
        (Some(r), _) => a.push(r.into()),
        (None, None) => a.push("--all".into()),
        (None, Some(_)) => {}
    }
    a.push("--".into());
    if let Some(p) = path {
        a.push(p.into());
    }
    a
}

pub struct LogView {
    /// 下の欄の「Git」タブに収めている（全画面でない）
    pub docked: bool,
    pub rows: Vec<LogRow>,
    /// 見せている行（rows の番号。絞り込み中はコミットの行だけ）
    pub visible: Vec<usize>,
    pub selected: usize,
    /// 複数選んだコミット（ハッシュ。Ctrl・Shift+クリック、Space）。空なら、選んでいるのは selected の 1 つだけ
    pub marked: Vec<String>,
    /// Shift+クリックで範囲を選ぶ起点（visible の番号）
    mark_anchor: Option<usize>,
    pub top: usize,
    /// 真ん中の列（グラフ・ref・題名）の左端の列。作者・日時・ハッシュは右に留めたまま
    pub left: usize,
    /// (見出し, 名前, 今のブランチか)
    pub branches: Vec<(String, String, bool)>,
    pub detail: Option<Detail>,
    pub focus_files: bool,
    pub file_sel: usize,
    /// 絞り込み（決めた文字）と、打っている間の Vim 風の入力欄
    pub filter_text: String,
    pub filter: Option<crate::vim_field::VimField>,
    /// ファイルの履歴なら、そのパス
    pub path: Option<String>,
    pub loading: bool,
    /// ブランチの列で選んでいるもの（branches の番号）
    pub branch_sel: Option<usize>,
    /// ブランチの列の先頭に見せている行（見出しの行も数える）
    pub branch_top: usize,
    /// ブランチの列の区分・フォルダの開き具合（BranchRow::Folder の key。初めは全部開く）
    pub branch_folds: nagi_ui::Folds<String>,
    /// 右の中身の列の先頭に見せている行と、変えたファイルの左端の列（長いパスを横に送って見る）
    pub detail_top: usize,
    pub detail_left: usize,
    /// 中身に合わせた作者・日時・ハッシュの列の幅（履歴が届いたときに測る）
    pub auto_cols: (usize, usize, usize),
    /// 読み終えたら選ぶコミット（ハッシュ。注釈の「VCS ログで表示」）
    pub want_hash: Option<String>,
    /// 読み直したあとも見ている所を変えない（続きを読んだとき）
    keep_view: bool,
    /// ブランチ・ユーザー・日付の絞り込みと、読むコミットの数
    pub query: LogQuery,
    pub limit: usize,
    /// 右の列の行の覚え（(コミット, 比べる相手, 幅) → 行）。折り返しを毎フレームやり直さない
    detail_cache: std::cell::RefCell<Option<((String, Option<String>, usize), std::rc::Rc<Vec<DetailLine>>)>>,
    /// 読み直すための元（ファイルの履歴のパス・比べる範囲）
    src_path: Option<String>,
    src_range: Option<String>,
    rx_log: Rx<Vec<LogRow>>,
    rx_branches: Rx<Vec<(String, String, bool)>>,
    rx_detail: Rx<Detail>,
}

/// 0 行目の絞り込みの字の欄の幅（その右に札を並べる）
pub const LOG_FILTER_W: usize = 22;

/// UNIX 時刻なら「3 日前」の形に（そうでなければそのまま）
fn when(s: &str) -> String {
    match s.trim().parse::<i64>() {
        Ok(t) => crate::blame::relative_time(t, crate::blame::now_secs()),
        Err(_) => s.to_string(),
    }
}

/// `git log --graph --format=<区切り付き>` を読む（グラフだけの行も残す）
pub fn parse_log(out: &str) -> Vec<LogRow> {
    out.lines()
        .map(|l| match l.split_once('\x1f') {
            Some((graph, rest)) => {
                let f: Vec<&str> = rest.split('\x1f').collect();
                let get = |i: usize| f.get(i).map_or(String::new(), |s| s.to_string());
                LogRow { graph: graph.trim_end().to_string(), commit: Some(Commit { short: get(0), refs: get(1), subject: get(2), author: get(3), date: when(&get(4)), hash: get(5) }) }
            }
            None => LogRow { graph: l.trim_end().to_string(), commit: None },
        })
        .collect()
}

/// `git show --name-status --format=...` を読む
pub fn parse_detail(out: &str) -> Detail {
    let (head, files) = out.split_once('\x1e').unwrap_or((out, ""));
    let f: Vec<&str> = head.split('\x1f').collect();
    let get = |i: usize| f.get(i).map_or(String::new(), |s| s.trim().to_string());
    let files = files
        .lines()
        .filter_map(|l| {
            let mut p = l.split('\t');
            let st = p.next()?.chars().next()?;
            let path = p.last()?.to_string();
            Some((st, path))
        })
        .collect();
    Detail { hash: get(0), author: get(1), date: when(&get(2)), parents: get(3), message: get(4), files, base: None }
}

/// グラフの文字を罫線にする
pub fn pretty_graph(g: &str) -> String {
    g.chars()
        .map(|c| match c {
            '*' => '●',
            '|' => '│',
            '/' => '╱',
            '\\' => '╲',
            '_' => '─',
            c => c,
        })
        .collect()
}

fn spawn<T: Send + 'static>(waker: Option<std::sync::Arc<dyn Fn() + Send + Sync>>, f: impl FnOnce() -> nagi_core::Result<T> + Send + 'static) -> Rx<T> {
    let rx = nagi_core::task::spawn("git-log", waker.unwrap_or_else(nagi_core::task::no_waker), move |ctx| {
        ctx.send(f());
    });
    Some(rx)
}

impl LogView {
    /// ブランチの列の行（見出しの下を 0 とする）→ ブランチの番号（区分・フォルダの行は None）
    #[cfg(test)]
    pub fn branch_at(&self, row: usize) -> Option<usize> {
        match self.branch_rows().get(self.branch_top + row) {
            Some(BranchRow::Leaf { i, .. }) => Some(*i),
            _ => None,
        }
    }

    /// 0 行目の絞り込みの札（見出し, 列, 幅, 種類）。絞り込みの字の欄の右に並べる
    pub fn chips(&self) -> Vec<(String, usize, usize, LogChip)> {
        let all = "すべて";
        let items = [
            (format!("ブランチ: {} ▾", self.query.branch.as_deref().unwrap_or(all)), LogChip::Branch),
            (format!("ユーザー: {} ▾", self.query.user.as_deref().unwrap_or(all)), LogChip::User),
            (format!("日付: {} ▾", self.query.period.and_then(|i| LOG_PERIODS.get(i)).map_or(all, |p| p.0)), LogChip::Date),
        ];
        let mut x = LOG_FILTER_W;
        items
            .into_iter()
            .map(|(label, c)| {
                let w = nagi_core::display_col(&label, label.len(), false);
                let at = x;
                x += w + 2;
                (label, at, w, c)
            })
            .collect()
    }

    /// ブランチの列の行。区分（ローカル・リモート・タグ）の下に、名前を / で分けたフォルダの木（IntelliJ と同じ）。
    /// 畳んだ区分・フォルダの中は出さない。フォルダはその段のブランチより先に並べる
    pub fn branch_rows(&self) -> Vec<BranchRow> {
        let mut out = Vec::new();
        let mut secs: Vec<&str> = Vec::new();
        for (sec, _, _) in &self.branches {
            if !secs.contains(&sec.as_str()) {
                secs.push(sec);
            }
        }
        for sec in secs {
            out.push(BranchRow::Folder { key: sec.to_string(), name: sec.to_string(), depth: 0 });
            if self.branch_folds.is_open(&sec.to_string()) {
                let names: Vec<(usize, &str)> = self.branches.iter().enumerate().filter(|(_, b)| b.0 == sec).map(|(i, b)| (i, b.1.as_str())).collect();
                out.extend(nagi_ui::tree::path_rows(&names, '/', &format!("{sec}/"), 1, &self.branch_folds));
            }
        }
        out
    }

    /// 割り付け（部品の中の座標。幅 `w`・高さ `h`、左の列と右の列の幅 `(bw, dw)`）。0 行目は絞り込み、
    /// 1 行目から左にブランチ、真ん中にコミット、右に中身。列の間の 1 列は区切り（どれにも当たらない）
    pub fn ui(&self, w: usize, h: usize, (bw, dw): (usize, usize)) -> nagi_ui::Ui<LogHit> {
        use nagi_ui::Rect;
        let mut ui = nagi_ui::Ui::default();
        let body = h.saturating_sub(1);
        for (_, x, cw, c) in self.chips() {
            ui.put(Rect::row(0, x, cw), LogHit::Chip(c));
        }
        ui.put(Rect::row(0, 0, w), LogHit::Filter);
        for (k, b) in self.branch_rows().into_iter().skip(self.branch_top).take(body).enumerate() {
            ui.put(Rect::row(1 + k, 0, bw), LogHit::Branch(b));
        }
        ui.put(Rect { x: 0, y: 1, w: bw, h: body }, LogHit::BranchArea);
        let cx = bw + 1;
        let cw = w.saturating_sub(bw + dw + 2);
        for (k, &i) in self.visible.iter().enumerate().skip(self.top).take(body) {
            let hit = if self.rows[i].commit.is_some() { LogHit::Commit(k) } else { LogHit::Graph(k) };
            ui.put(Rect::row(1 + k - self.top, cx, cw), hit);
        }
        ui.put(Rect { x: cx, y: 1, w: cw, h: body }, LogHit::CommitArea);
        let dx = cx + cw + 1;
        if let Some(d) = &self.detail {
            for (k, line) in self.lines_of(d, dw.saturating_sub(1)).iter().skip(self.detail_top).take(body).enumerate() {
                ui.put(Rect::row(1 + k, dx, w.saturating_sub(dx)), LogHit::Detail(line.clone()));
            }
        }
        ui.put(Rect { x: dx, y: 1, w: w.saturating_sub(dx), h: body }, LogHit::DetailArea);
        ui
    }

    /// 絞り込みの欄の外のキー（`rows` は見える行数）。部品の中で済むこと（選び・複数選び・ファイルへ移る・絞り込みを開く）は
    /// ここで行い、部品の外に及ぶこと（本文へ・欄を隠す・閉じる・差分・確認・`:` の行・コピー）を返す（ADR 0018 の §5）
    pub fn key(&mut self, key: Key, rows: usize, svc: &mut crate::editor::Services) -> LogAction {
        // 絞り込みの欄（Vim 風の入力欄）
        if let Some(f) = &mut self.filter {
            use crate::vim_field::FieldOut;
            match f.feed(key, svc) {
                FieldOut::Changed => {
                    self.filter_text = f.text();
                    self.refilter();
                    self.detail = None;
                }
                FieldOut::Submit | FieldOut::Cancel => self.filter = None,
                FieldOut::None => {}
            }
            return LogAction::None;
        }
        // 複数選んでいれば、Esc はまず選びを外す
        if key == Key::Esc && !self.focus_files && !self.marked.is_empty() {
            self.marked.clear();
            self.mark_anchor = None;
            return LogAction::None;
        }
        // 下の欄に収めているとき: Esc は本文へ戻るだけ（履歴は残す）
        if self.docked && !self.focus_files && key == Key::Esc {
            return LogAction::ToText;
        }
        // 標準のキーだけ（ADR 0019。チェックアウト・チェリーピック・コピーなどはアクション）
        if self.focus_files {
            let n = self.detail.as_ref().map_or(0, |d| d.files.len());
            return match key {
                Key::Down => {
                    self.file_sel = (self.file_sel + 1).min(n.saturating_sub(1));
                    LogAction::FitDetail
                }
                Key::Up => {
                    self.file_sel = self.file_sel.saturating_sub(1);
                    LogAction::FitDetail
                }
                Key::Tab | Key::Esc | Key::Left => {
                    self.focus_files = false;
                    LogAction::None
                }
                Key::Enter => LogAction::OpenFileDiff,
                _ => LogAction::None,
            };
        }
        // 移動は一覧の共通の動き（nagi_ui::list）。グラフだけの行は飛ばす
        let n = self.visible.len();
        if let Some(to) = nagi_ui::list::nav(key, self.selected, n, rows) {
            self.selected = nagi_ui::list::snap(self.selected, to, n, |i| self.rows[self.visible[i]].commit.is_some());
            return LogAction::None;
        }
        let act = match key {
            Key::Tab | Key::Enter | Key::Right => {
                if self.detail.as_ref().is_some_and(|d| !d.files.is_empty()) {
                    self.focus_files = true;
                }
                LogAction::None
            }
            Key::Char(' ') => {
                let k = self.selected;
                self.toggle_mark(k);
                self.mark_anchor = Some(k);
                LogAction::None
            }
            // 打った字は絞り込みの欄へ（IntelliJ のスピードサーチと同じ）
            Key::Char(_) => {
                let mut f = crate::vim_field::VimField::new(&self.filter_text, false);
                f.feed(key, svc);
                self.filter_text = f.text();
                self.filter = Some(f);
                self.refilter();
                self.detail = None;
                LogAction::None
            }
            Key::Esc => LogAction::Close,
            _ => LogAction::None,
        };
        nagi_ui::list::fit(self.selected, &mut self.top, rows);
        act
    }

    /// 右の列の行（detail_lines を、コミット・比べる相手・幅が同じあいだは覚えて使う）
    fn lines_of(&self, d: &Detail, w: usize) -> std::rc::Rc<Vec<DetailLine>> {
        let key = (d.hash.clone(), d.base.clone(), w);
        if let Some((k, v)) = self.detail_cache.borrow().as_ref() {
            if *k == key {
                return v.clone();
            }
        }
        let v = std::rc::Rc::new(detail_lines(d, w));
        *self.detail_cache.borrow_mut() = Some((key, v.clone()));
        v
    }

    /// 区分・フォルダを畳む・開く
    pub fn toggle_branch_folder(&mut self, key: &str) {
        self.branch_folds.toggle(&key.to_string());
        let n = self.branch_rows().len();
        self.branch_top = self.branch_top.min(n.saturating_sub(1));
    }

    /// 名前（ブランチ・タグ）の指すコミットの行（見せている行の番号）
    pub fn row_of_ref(&self, name: &str) -> Option<usize> {
        self.visible.iter().position(|&i| {
            self.rows[i].commit.as_ref().is_some_and(|c| c.refs.split(", ").any(|r| r.trim_start_matches("HEAD -> ").trim_start_matches("tag: ") == name))
        })
    }

    fn current(&self) -> Option<&Commit> {
        self.visible.get(self.selected).and_then(|&i| self.rows[i].commit.as_ref())
    }

    fn commit_at(&self, k: usize) -> Option<&Commit> {
        self.visible.get(k).and_then(|&i| self.rows[i].commit.as_ref())
    }

    /// 選んでいるコミット（複数選んでいればそのすべて、でなければ今の 1 つ）。並びは履歴の順（新しい方が先）
    pub fn picked(&self) -> Vec<Commit> {
        if self.marked.is_empty() {
            return self.current().cloned().into_iter().collect();
        }
        self.visible.iter().filter_map(|&i| self.rows[i].commit.as_ref()).filter(|c| self.marked.contains(&c.hash)).cloned().collect()
    }

    /// k 番目（visible の番号）を選びに足す・外す（Ctrl+クリック）。`range` なら起点から k までを選ぶ（Shift+クリック）
    fn mark_click(&mut self, k: usize, range: bool) {
        if range {
            let a = self.mark_anchor.unwrap_or(self.selected);
            let (lo, hi) = (a.min(k), a.max(k));
            self.marked = (lo..=hi).filter_map(|i| self.commit_at(i).map(|c| c.hash.clone())).collect();
            self.mark_anchor = Some(a);
        } else {
            // 初めの Ctrl+クリックでは、今選んでいる 1 つも選びに入れる（IntelliJ と同じ）
            if self.marked.is_empty() {
                self.marked.extend(self.current().map(|c| c.hash.clone()));
            }
            self.toggle_mark(k);
            self.mark_anchor = Some(k);
        }
        self.selected = k;
    }

    /// k 番目を選びに足す・外す
    fn toggle_mark(&mut self, k: usize) {
        let Some(h) = self.commit_at(k).map(|c| c.hash.clone()) else { return };
        match self.marked.iter().position(|m| *m == h) {
            Some(i) => {
                self.marked.remove(i);
            }
            None => self.marked.push(h),
        }
    }

    pub fn refilter_pub(&mut self) {
        self.refilter();
        self.detail = None;
    }

    fn refilter(&mut self) {
        let q = self.filter_text.to_lowercase();
        self.visible = if q.is_empty() {
            (0..self.rows.len()).collect()
        } else {
            (0..self.rows.len())
                .filter(|&i| {
                    self.rows[i].commit.as_ref().is_some_and(|c| {
                        c.subject.to_lowercase().contains(&q) || c.author.to_lowercase().contains(&q) || c.short.starts_with(&q) || c.refs.to_lowercase().contains(&q)
                    })
                })
                .collect()
        };
        self.selected = self.visible.iter().position(|&i| self.rows[i].commit.is_some()).unwrap_or(0);
        self.top = 0;
        // 絞り込みで見えなくなったコミットに操作が及ばないよう、選びは外す
        self.marked.clear();
        self.mark_anchor = None;
    }
}

impl Editor {
    /// `:Glog`（`file` ならそのファイルの履歴）
    pub(crate) fn open_log(&mut self, file: bool) {
        self.open_log_with(file, None);
    }

    /// 2 つのブランチの差（`main..topic`: topic にだけあるコミット）
    pub(crate) fn open_log_range(&mut self, range: &str) {
        self.open_log_with(false, Some(range.to_string()));
    }

    fn open_log_with(&mut self, file: bool, range: Option<String>) {
        if self.svc.git.root.is_none() {
            self.start_git();
        }
        let Some(root) = self.svc.git.root.clone() else {
            self.svc.notify.info("git のリポジトリではありません");
            return;
        };
        let path = if file {
            let Some(p) = self.core.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok()) else { return };
            p.strip_prefix(&root).ok().map(|r| r.to_string_lossy().replace('\\', "/"))
        } else {
            None
        };
        // 新しく開く（下の欄に収めるのは select_bottom_tab が後で決める）
        self.panels.log = None;
        self.load_log(root, path, range);
    }

    /// 履歴を読み直す（操作の後。下の欄に収めているかは保つ）
    pub(crate) fn reload_log(&mut self) {
        let (Some(root), Some(l)) = (self.svc.git.root.clone(), &self.panels.log) else { return };
        let (path, range) = (l.src_path.clone(), l.src_range.clone());
        let (query, limit) = (l.query.clone(), l.limit);
        let state = self.state();
        self.load_log(root, path, range);
        if let Some(l) = &mut self.panels.log {
            if query != LogQuery::default() || limit != LOG_PAGE {
                l.query = query;
                l.limit = limit;
                self.log_requery(None, false);
            }
        }
        self.set_state(state);
    }

    /// 同じ履歴を、今の絞り込み・数で読み直す（選んでいるコミットはそのまま。`keep` なら見ている所も変えない）
    pub(crate) fn log_requery(&mut self, want: Option<String>, keep: bool) {
        let (Some(root), Some(l)) = (self.svc.git.root.clone(), &mut self.panels.log) else { return };
        l.want_hash = want.or_else(|| l.want_hash.clone()).or_else(|| l.current().map(|c| c.hash.clone()));
        l.keep_view = keep;
        l.loading = true;
        let args = log_args(l.src_path.as_deref(), l.src_range.as_deref(), &l.query, l.limit);
        l.rx_log = spawn(self.waker.clone(), move || {
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            let out = nagi_workers::git::git(&root, &a)?;
            Ok(parse_log(&String::from_utf8_lossy(&out)))
        });
    }

    /// 札を押した: 選ぶメニュー（ブランチ・ユーザー・日付）
    pub(crate) fn log_chip_menu(&mut self, c: LogChip, at: (usize, usize)) {
        let Some(l) = &self.panels.log else { return };
        let mark = |on: bool| if on { "✓ " } else { "   " };
        let mut m = crate::menu::Menu::new(at.0, at.1);
        let set = |f: fn(&mut LogQuery)| move |e: &mut Editor| {
            if let Some(l) = &mut e.panels.log {
                f(&mut l.query);
                l.limit = LOG_PAGE;
            }
            e.log_requery(None, false);
        };
        match c {
            LogChip::Branch => {
                m = m.item(format!("{}すべて", mark(l.query.branch.is_none())), set(|q| q.branch = None)).sep();
                for (sec, name, _) in l.branches.iter().filter(|b| b.0 != "タグ").take(40) {
                    let n = name.clone();
                    let _ = sec;
                    m = m.item(format!("{}{name}", mark(l.query.branch.as_deref() == Some(name))), move |e| {
                        if let Some(l) = &mut e.panels.log {
                            l.query.branch = Some(n.clone());
                            l.limit = LOG_PAGE;
                        }
                        e.log_requery(None, false);
                    });
                }
            }
            LogChip::User => {
                // 読んだコミットの作者（多い順）
                let mut count: Vec<(String, usize)> = Vec::new();
                for c in l.rows.iter().filter_map(|r| r.commit.as_ref()) {
                    match count.iter_mut().find(|x| x.0 == c.author) {
                        Some(x) => x.1 += 1,
                        None => count.push((c.author.clone(), 1)),
                    }
                }
                count.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                if let Some(u) = l.query.user.clone().filter(|u| !count.iter().any(|x| &x.0 == u)) {
                    count.insert(0, (u, 0));
                }
                m = m.item(format!("{}すべて", mark(l.query.user.is_none())), set(|q| q.user = None)).sep();
                for (name, _) in count.into_iter().take(30) {
                    let on = l.query.user.as_deref() == Some(name.as_str());
                    m = m.item(format!("{}{name}", mark(on)), move |e| {
                        if let Some(l) = &mut e.panels.log {
                            l.query.user = Some(name.clone());
                            l.limit = LOG_PAGE;
                        }
                        e.log_requery(None, false);
                    });
                }
            }
            LogChip::Date => {
                m = m.item(format!("{}すべて", mark(l.query.period.is_none())), set(|q| q.period = None)).sep();
                for (i, (label, _)) in LOG_PERIODS.iter().enumerate() {
                    m = m.item(format!("{}{label}", mark(l.query.period == Some(i))), move |e| {
                        if let Some(l) = &mut e.panels.log {
                            l.query.period = Some(i);
                            l.limit = LOG_PAGE;
                        }
                        e.log_requery(None, false);
                    });
                }
            }
        }
        self.open_menu(m);
    }

    /// 見えている所が読んだ分の終わりに近づいたら、続きを読む（数を倍にして読み直す）
    pub(crate) fn log_more_if_needed(&mut self, body: usize) {
        let Some(l) = &self.panels.log else { return };
        let commits = l.rows.iter().filter(|r| r.commit.is_some()).count();
        if l.loading || commits < l.limit || l.limit >= LOG_MAX || l.top + body * 2 < l.visible.len() {
            return;
        }
        if let Some(l) = &mut self.panels.log {
            l.limit = (l.limit * 2).min(LOG_MAX);
        }
        self.log_requery(None, true);
    }

    /// 履歴を（読み直すときも）裏で取る
    fn load_log(&mut self, root: std::path::PathBuf, path: Option<String>, range: Option<String>) {
        let docked = self.panels.log.as_ref().is_some_and(|l| l.docked);
        let r2 = root.clone();
        let p2 = path.clone();
        let range2 = range.clone();
        let rx_log = spawn(self.waker.clone(), move || {
            let args = log_args(p2.as_deref(), range2.as_deref(), &LogQuery::default(), LOG_PAGE);
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            let out = nagi_workers::git::git(&r2, &a)?;
            Ok(parse_log(&String::from_utf8_lossy(&out)))
        });
        let r3 = root.clone();
        let rx_branches = spawn(self.waker.clone(), move || {
            let out = nagi_workers::git::git(&r3, &["for-each-ref", "--format=%(HEAD)\x1f%(refname)\x1f%(refname:short)", "refs/heads", "refs/remotes", "refs/tags"])?;
            let mut v = Vec::new();
            for l in String::from_utf8_lossy(&out).lines() {
                let f: Vec<&str> = l.split('\x1f').collect();
                // origin/HEAD は除く（短い名前だと「origin」になるので、元の名前で見る）
                if f.len() < 3 || f[1].ends_with("/HEAD") {
                    continue;
                }
                let sec = if f[1].starts_with("refs/heads") {
                    "ローカル"
                } else if f[1].starts_with("refs/remotes") {
                    "リモート"
                } else {
                    "タグ"
                };
                v.push((sec.to_string(), f[2].to_string(), f[0] == "*"));
            }
            Ok(v)
        });
        self.panels.log = Some(LogView {
            left: 0,
            docked,
            branch_sel: None,
            branch_top: 0,
            branch_folds: nagi_ui::Folds::open(),
            detail_top: 0,
            detail_left: 0,
            auto_cols: (10, 10, 7),
            want_hash: None,
            keep_view: false,
            query: LogQuery::default(),
            limit: LOG_PAGE,
            detail_cache: Default::default(),
            src_path: path.clone(),
            src_range: range.clone(),
            rows: Vec::new(),
            visible: Vec::new(),
            selected: 0,
            marked: Vec::new(),
            mark_anchor: None,
            top: 0,
            branches: Vec::new(),
            detail: None,
            focus_files: false,
            file_sel: 0,
            filter_text: String::new(),
            filter: None,
            path: path.or(range.map(|r| if r.contains("..") { format!("比較 {r}") } else { format!("ブランチ {r}") })),
            loading: true,
            rx_log,
            rx_branches,
            rx_detail: None,
        });
        self.set_state(State::Log);
    }

    /// 裏で取ったログ・ブランチ・コミットの中身を受け取る。変わったら true
    pub fn poll_log(&mut self) -> bool {
        let Some(l) = &mut self.panels.log else { return false };
        let mut changed = false;
        if let Some(Ok(r)) = l.rx_log.as_ref().map(|rx| rx.try_recv()) {
            l.rx_log = None;
            l.loading = false;
            match r {
                Ok(rows) => {
                    l.auto_cols = auto_cols(&rows);
                    let commits = rows.iter().filter(|r| r.commit.is_some()).count();
                    l.rows = rows;
                    l.refilter();
                    let keep = std::mem::take(&mut l.keep_view);
                    if let Some(h) = l.want_hash.take() {
                        match l.visible.iter().position(|&i| l.rows[i].commit.as_ref().is_some_and(|c| c.hash.starts_with(&h))) {
                            Some(k) => {
                                l.selected = k;
                                if !keep {
                                    l.top = k.saturating_sub(5);
                                }
                            }
                            // 読んだ分に無ければ、数を増やして探す（注釈の「VCS ログで表示」で古いコミットを開いたとき）
                            None if commits >= l.limit && l.limit < LOG_MAX => {
                                l.limit = (l.limit * 4).min(LOG_MAX);
                                self.log_requery(Some(h), false);
                                return true;
                            }
                            None if keep => {}
                            None => self.svc.notify.info(format!("{} は履歴（新しい {commits} 件）にありません", &h[..7.min(h.len())])),
                        }
                    }
                }
                Err(e) => self.svc.notify.error(format!("git log: {}", e.first_line())),
            }
            changed = true;
        }
        let Some(l) = &mut self.panels.log else { return changed };
        if let Some(Ok(r)) = l.rx_branches.as_ref().map(|rx| rx.try_recv()) {
            l.rx_branches = None;
            if let Ok(b) = r {
                l.branches = b;
            }
            changed = true;
        }
        if let Some(Ok(r)) = l.rx_detail.as_ref().map(|rx| rx.try_recv()) {
            l.rx_detail = None;
            if let Ok(d) = r {
                // 届くまでに選びが変わっていたら捨てる
                if l.current().is_some_and(|c| c.hash == d.hash) {
                    l.detail = Some(d);
                    l.file_sel = 0;
                    l.detail_top = 0;
                    l.detail_left = 0;
                }
            }
            changed = true;
        }
        self.log_request_detail();
        changed
    }

    /// 選んでいるコミットの中身がまだなら、裏で取りに行く。描く前にも呼ぶ（選びはキー・マウスで変わり、
    /// 裏の仕事が終わったときだけ見ていると、ほかの仕事が無い間は前のコミットの中身のままになる）
    pub(crate) fn log_request_detail(&mut self) {
        let waker = self.waker.clone();
        let root = self.svc.git.root.clone();
        let Some(l) = &mut self.panels.log else { return };
        let want = l.current().map(|c| c.hash.clone());
        if let (Some(h), Some(root)) = (want, root) {
            if l.detail.as_ref().is_none_or(|d| d.hash != h) && l.rx_detail.is_none() {
                l.rx_detail = spawn(waker, move || {
                    let out = nagi_workers::git::git(&root, &["show", "--name-status", "--format=%H\x1f%an\x1f%at\x1f%P\x1f%B\x1e", &h])?;
                    Ok(parse_detail(&String::from_utf8_lossy(&out)))
                });
            }
        }
    }

    /// 履歴の画面のキー
    pub(crate) fn log_key(&mut self, key: Key) {
        self.log_key_inner(key);
        let rows = self.log_rows();
        self.log_more_if_needed(rows);
    }

    fn log_key_inner(&mut self, key: Key) {
        let rows = self.log_rows();
        let Some(l) = &mut self.panels.log else {
            self.set_state(State::Normal);
            return;
        };
        // 部品の中で済むこと（絞り込みの欄を含む）は LogView::key が部品の状態と Services で行い、外に及ぶことを返す（ADR 0018 の §5）
        let a = l.key(key, rows, &mut self.svc);
        self.log_act(a);
    }

    /// 履歴の操作を行う（キー・アクションから）
    pub(crate) fn log_act(&mut self, a: LogAction) {
        match a {
            LogAction::None => {}
            LogAction::ToText => self.set_state(State::Normal),
            LogAction::HideBottom => self.hide_bottom(),
            LogAction::Close => self.close_log(),
            LogAction::FitDetail => self.log_fit_detail(),
            LogAction::OpenFileDiff => self.log_open_file_diff(),
            LogAction::AskCheckout(h) => self.log_ask(Confirm::Checkout(h)),
            LogAction::CherryPick => self.log_cherry_pick_picked(),
            LogAction::PromptBranch(h) => self.prompt_command("Gbranch ", &format!(" {h}")),
            LogAction::Copy => self.log_copy_picked(false),
        }
    }

    /// 履歴の中の (行, 列) にあるもの（幅 `w`。描き方と同じ割り付け）
    fn log_hit(&self, row: usize, col: usize, w: usize) -> Option<LogHit> {
        let h = self.log_rect().map_or(row + 1, |r| r.h);
        self.panels.log.as_ref()?.ui(w, h, self.log_cols_for(w)).hit(row, col).map(|h| h.0.clone())
    }

    /// 選んでいるファイルの行が、右の中身の列の見える範囲に入るように送る
    fn log_fit_detail(&mut self) {
        let rows = self.log_rows();
        let Some(w) = self.log_rect().map(|r| r.w) else { return };
        let dw = self.log_cols_for(w).1;
        let Some(l) = &mut self.panels.log else { return };
        let Some(d) = &l.detail else { return };
        if let Some(k) = detail_lines(d, dw.saturating_sub(1)).iter().position(|x| *x == DetailLine::File(l.file_sel)) {
            nagi_ui::list::fit(k, &mut l.detail_top, rows);
        }
    }

    /// 下の欄の Git を押した（欄の中身の行・列・幅。0 行目は絞り込みの見出し）。
    /// 真ん中はコミットを選ぶ（ダブルクリックで変えたファイルへ）、右は変えたファイルを選ぶ（ダブルクリックで差分）
    pub(crate) fn log_click(&mut self, row: usize, col: usize, w: usize, clicks: u32) {
        self.set_state(State::Log);
        let mods = self.click_mods;
        // 描き方（draw_log_view）と同じ割り付けで当てる
        let hit = self.log_hit(row, col, w);
        if let Some(LogHit::Chip(c)) = hit {
            let r = self.log_rect().unwrap_or_default();
            let x = self.panels.log.as_ref().and_then(|l| l.chips().into_iter().find(|x| x.3 == c)).map_or(col, |x| x.1);
            self.log_chip_menu(c, (r.y + row + 1, r.x + x));
            return;
        }
        let Some(l) = &mut self.panels.log else { return };
        if hit == Some(LogHit::Filter) {
            // 0 行目（「/ で絞り込み」の見出し）を押したら絞り込みの欄を開く
            if l.filter.is_none() {
                l.filter = Some(crate::vim_field::VimField::new(&l.filter_text, false));
            }
            return;
        }
        l.filter = None;
        match hit {
            Some(LogHit::Commit(k)) => {
                // Ctrl+クリックで 1 つずつ足す・外す、Shift+クリックで範囲を選ぶ
                if mods.ctrl || mods.shift {
                    l.mark_click(k, mods.shift);
                    l.focus_files = false;
                    return;
                }
                let pick = nagi_ui::list::pick(&mut l.selected, Some(k), clicks, 2);
                if pick.is_some() {
                    l.focus_files = false;
                    l.marked.clear();
                    l.mark_anchor = Some(l.selected);
                }
                if matches!(pick, Some(nagi_ui::list::Pick::Accept(_))) && l.detail.as_ref().is_some_and(|d| !d.files.is_empty()) {
                    l.focus_files = true;
                }
            }
            // 右: 中身のうち、ファイルの行
            Some(LogHit::Detail(DetailLine::File(f))) => {
                let pick = nagi_ui::list::pick(&mut l.file_sel, Some(f), clicks, 2);
                if pick.is_some() {
                    l.focus_files = true;
                }
                if matches!(pick, Some(nagi_ui::list::Pick::Accept(_))) {
                    self.log_open_file_diff();
                }
            }
            // 左: 区分・フォルダは 1 回のクリックで畳む・開く（プロジェクトの木と同じ）
            Some(LogHit::Branch(BranchRow::Folder { key, .. })) => {
                l.focus_files = false;
                l.toggle_branch_folder(&key);
            }
            // 左: ブランチ。押すとそのブランチの指すコミットへ、ダブルクリックでそのブランチの履歴だけを出す
            // （IntelliJ と同じ。チェックアウトは右クリックのメニューから）
            Some(LogHit::Branch(BranchRow::Leaf { i: b, .. })) => {
                l.focus_files = false;
                let mut i = l.branch_sel.unwrap_or(0);
                let Some(pick) = nagi_ui::list::pick(&mut i, Some(b), clicks, 2) else { return };
                l.branch_sel = Some(i);
                if let Some(k) = l.row_of_ref(&l.branches[i].1) {
                    l.selected = k;
                    let rows = self.log_rows();
                    if let Some(l) = &mut self.panels.log {
                        nagi_ui::list::fit(k, &mut l.top, rows);
                    }
                }
                if matches!(pick, nagi_ui::list::Pick::Accept(_)) {
                    self.log_show_branch(i);
                }
            }
            Some(LogHit::BranchArea) => l.focus_files = false,
            // グラフだけの行・中身の見出しなど・空いた所は選ばない
            Some(LogHit::Graph(_) | LogHit::CommitArea | LogHit::Detail(_) | LogHit::DetailArea | LogHit::Filter | LogHit::Chip(_)) => {}
            // 列の間の区切り
            None => l.focus_files = false,
        }
    }

    /// そのブランチの履歴だけを出す。もう出していれば、全部のブランチに戻す（下の欄か窓の領域かは保つ）
    pub(crate) fn log_show_branch(&mut self, i: usize) {
        let Some(l) = &mut self.panels.log else { return };
        let Some(name) = l.branches.get(i).map(|b| b.1.clone()) else { return };
        let all = l.src_range.as_deref() == Some(name.as_str());
        l.src_range = if all { None } else { Some(name.clone()) };
        l.src_path = None;
        self.reload_log();
        if let Some(l) = &mut self.panels.log {
            l.branch_sel = Some(i);
        }
        self.svc.notify.info(if all { "全部のブランチの履歴を出しました".to_string() } else { format!("{name} の履歴だけを出しました（もう一度ダブルクリックで全部に戻す）") });
    }

    /// `:` の行に `head` と `tail` を入れ、その間にカーソルを置く（名前などを打たせる）
    fn prompt_command(&mut self, head: &str, tail: &str) {
        self.enter_command("");
        self.cmdline.set(&format!("{head}{tail}"));
        self.cmdline.cursor = head.len();
        self.update_cmd_complete();
    }

    /// 確かめてから行う（確認の窓で「はい」なら log_do）
    pub(crate) fn log_ask(&mut self, c: Confirm) {
        let s = |h: &str| h[..7.min(h.len())].to_string();
        let cur = self.svc.git.status.as_ref().map(|s| s.branch.clone()).unwrap_or_default();
        let q = match &c {
            Confirm::Checkout(h) => format!("{} をチェックアウトしますか（HEAD を切り離す）", s(h)),
            Confirm::CherryPick(h) => format!("{} を {cur} にチェリーピックしますか", s(h)),
            Confirm::Revert(h) => format!("{} を取り消すコミットを足しますか（revert）", s(h)),
            Confirm::CherryPickMany(hs) => format!("{} 件のコミットを {cur} にチェリーピックしますか（古い順に。衝突したら全部やめて元に戻す）", hs.len()),
            Confirm::RevertMany(hs) => format!("{} 件のコミットを取り消すコミットを足しますか（revert。新しい順に。衝突したら全部やめて元に戻す）", hs.len()),
            Confirm::Undo(h) => format!("{} を取り消して、変更は作業ツリーに残しますか", s(h)),
            Confirm::Squash(h, true) => format!("{} を 1 つ前のコミットにまとめますか（メッセージはつなぐ）", s(h)),
            Confirm::Squash(h, false) => format!("{} を 1 つ前のコミットにまとめますか（このメッセージは捨てる）", s(h)),
            Confirm::Drop(h) => format!("{} を消しますか（その後のコミットは載せ直す）", s(h)),
            Confirm::Reset(h, "--hard") => format!("{cur} を {} までリセットしますか（--hard: 作業中の変更も消える）", s(h)),
            Confirm::Reset(h, m) => format!("{cur} を {} までリセットしますか（{m}）", s(h)),
            Confirm::Rebase(h) => format!("{cur} を {} の上へリベースしますか", s(h)),
            Confirm::PushTo(h, up) => format!("{} までを {up} へ push しますか", s(h)),
            Confirm::DeleteBranch(b, false) => format!("ブランチ {b} を消しますか"),
            Confirm::DeleteBranch(b, true) => format!("ブランチ {b} はマージされていません。それでも消しますか"),
            Confirm::CheckoutRebase(b, onto) => format!("{b} をチェックアウトして {onto} の上へリベースしますか"),
        };
        self.ask(q, move |e| e.log_do(c));
    }

    /// 確かめた操作を行い、git の状態と履歴を読み直す。作業中の変更が上書きされるなら、退避・破棄・中止を確かめてもう一度
    fn log_do(&mut self, c: Confirm) {
        use nagi_workers::git;
        let Some(root) = self.svc.git.root.clone() else { return };
        match &c {
            Confirm::PushTo(h, up) => {
                // 送るのはネットワークを使うので裏で
                let (remote, branch) = up.split_once('/').unwrap_or(("origin", up));
                let refspec = format!("{h}:refs/heads/{branch}");
                self.run_git_job("push ", &["push", remote, &refspec]);
            }
            Confirm::DeleteBranch(b, force) => {
                let (b, force) = (b.clone(), *force);
                let name = b.clone();
                self.job("git", move || git::git(&root, &["branch", if force { "-D" } else { "-d" }, &name]), move |e, r| {
                    match r {
                        // マージされていなければ、もう一度確かめてから -D
                        Err(err) if !force && err.to_string().contains("not fully merged") => {
                            e.log_ask(Confirm::DeleteBranch(b, true));
                            return;
                        }
                        Ok(_) => e.svc.notify.info(format!("ブランチ {b} を消しました")),
                        Err(err) => e.svc.notify.error(format!("git: {}", git::reason(&err))),
                    }
                    e.refresh_git();
                    e.reload_log();
                });
            }
            _ => self.git_guarded(move |root| log_op(root, &c), |e, _| e.reload_log()),
        }
    }

    /// ブランチの列の i 番目をチェックアウトする（右クリックのメニュー）
    pub(crate) fn log_checkout_branch(&mut self, i: usize) {
        let Some((sec, name, cur)) = self.panels.log.as_ref().and_then(|l| l.branches.get(i).cloned()) else { return };
        if cur {
            self.svc.notify.info(format!("{name} はもうチェックアウトしています"));
            return;
        }
        if sec == "タグ" {
            self.log_ask(Confirm::Checkout(name));
            return;
        }
        let remote = sec == "リモート";
        let local = name.split_once('/').map_or(name.as_str(), |(_, n)| n).to_string();
        let exists = self.panels.log.as_ref().is_some_and(|l| l.branches.iter().any(|(s, n, _)| s == "ローカル" && *n == local));
        self.checkout_ref(&name, remote, exists, |e, _| e.reload_log());
    }

    /// ホイール: 表示だけを送る（選んでいるコミットは動かさない。キーで動かすと見える範囲へ戻る）。
    /// 横は真ん中の列（グラフ・ref・題名）を送る。`w` は履歴の幅、`col` は履歴の中のマウスの列（左のブランチの列・右の中身の列ならそこを送る）
    pub(crate) fn log_wheel(&mut self, dy: isize, dx: isize, w: usize, col: usize) {
        self.log_wheel_inner(dy, dx, w, col);
        let rows = self.log_rows();
        self.log_more_if_needed(rows);
    }

    fn log_wheel_inner(&mut self, dy: isize, dx: isize, w: usize, col: usize) {
        let rows = self.log_rows();
        let cols = self.log_cols_for(w);
        let right = self.log_right_cols();
        let Some(l) = &mut self.panels.log else { return };
        if col + cols.1 + 1 > w {
            // 右の中身: 縦は中身全体、横は変えたファイルの長いパス
            let Some(d) = &l.detail else { return };
            let tw = cols.1.saturating_sub(1);
            let lines = detail_lines(d, tw);
            l.detail_top = (l.detail_top as isize + dy).clamp(0, lines.len().saturating_sub(rows) as isize) as usize;
            if dx != 0 {
                let cw = |t: &str| nagi_core::display_col(t, t.len(), false);
                let widest = d.files.iter().map(|(st, p)| file_line(*st, p)).map(|(a, b)| cw(&a) + cw(&b)).max().unwrap_or(0);
                nagi_ui::list::hwheel(&mut l.detail_left, widest + 1, tw, dx);
            }
            return;
        }
        if col < cols.0 {
            let n = l.branch_rows().len();
            l.branch_top = (l.branch_top as isize + dy).clamp(0, n.saturating_sub(rows) as isize) as usize;
            return;
        }
        l.top = (l.top as isize + dy).clamp(0, l.visible.len().saturating_sub(rows) as isize) as usize;
        if dx != 0 {
            let widest = l.visible.iter().skip(l.top).take(rows).map(|&i| l.rows[i].text_width()).max().unwrap_or(0);
            nagi_ui::list::hwheel(&mut l.left, widest + 1, log_text_w(w, cols, right), dx);
        }
    }

    /// 履歴（幅 `w`）の左のブランチの列と右の中身の列の幅
    pub(crate) fn log_cols_for(&self, w: usize) -> (usize, usize) {
        log_cols(w, self.svc.sizes.get("log_branch_width"), self.svc.sizes.get("log_detail_width"))
    }

    /// コミットの作者・日時・ハッシュの列の幅（作者・日時はドラッグで変えていればその幅、でなければ中身に合わせる）
    pub(crate) fn log_right_cols(&self) -> (usize, usize, usize) {
        let auto = self.panels.log.as_ref().map_or((10, 10, 7), |l| l.auto_cols);
        (self.svc.sizes.get("log_author_width").unwrap_or(auto.0).clamp(4, 40), self.svc.sizes.get("log_date_width").unwrap_or(auto.1).clamp(4, 40), auto.2)
    }

    /// コミットの列（`end` はその右端の次の列）の、題名と作者の境目・作者と日時の境目の列（描き方と同じ割り付け）
    pub(crate) fn log_commit_seps(&self, end: usize) -> (usize, usize) {
        let (aw, dw, hw) = self.log_right_cols();
        (end.saturating_sub(aw + dw + hw + 4), end.saturating_sub(dw + hw + 3))
    }

    /// 履歴の区切り（見出しの行より下）: ブランチの列の右・中身の列の左・コミットの題名と作者の境目・作者と日時の境目（resize.rs が当てる）
    pub(crate) fn log_splitters(&self) -> Vec<nagi_ui::Splitter> {
        use nagi_ui::{Axis, Splitter};
        let Some(r) = self.log_rect() else { return Vec::new() };
        let (bw, dw) = self.log_cols_for(r.w);
        let (_, tw, hw) = self.log_right_cols();
        let (end, span) = (r.x + r.w, r.y + 1..r.y + r.h);
        // コミットの列の右端の次の列
        let cend = end.saturating_sub(dw + 1);
        let (author, date) = self.log_commit_seps(cend);
        let sep = |key, line, origin, forward, extra| Splitter { key, axis: Axis::Cols, line, span: span.clone(), origin, forward, extra };
        vec![
            sep("log_branch_width", r.x + bw, r.x, true, 0),
            sep("log_detail_width", cend, end, false, 1),
            sep("log_author_width", author, cend, false, 4 + tw + hw),
            sep("log_date_width", date, cend, false, 3 + hw),
        ]
    }

    /// 履歴を出している矩形（窓の領域いっぱいか、下の欄の中身）。区切りをつかむのに使う
    pub(crate) fn log_rect(&self) -> Option<crate::panes::Rect> {
        let l = self.panels.log.as_ref()?;
        if self.panels.diff.is_some() {
            return None;
        }
        if !l.docked {
            let (x, w) = self.windows_x();
            return Some(crate::panes::Rect { x, y: self.area().y, w, h: self.windows_rows().saturating_sub(self.area().y) });
        }
        (self.bottom_kind == crate::result_grid::BottomKind::Git).then(|| self.bottom_content_rect()).flatten()
    }

    /// 履歴の行の高さ（下の欄か窓の領域か）
    fn log_rows(&self) -> usize {
        if self.panels.log.as_ref().is_some_and(|l| l.docked) {
            self.bottom_content_rect().map_or(5, |r| r.h).saturating_sub(2).max(1)
        } else {
            self.windows_rows().saturating_sub(3).max(1)
        }
    }

    /// 右クリック（欄の中の行・列・幅と、メニューを出す画面の位置）。押した行を選んでから、その行のメニューを出す
    pub(crate) fn log_context(&mut self, row: usize, col: usize, w: usize, at: (usize, usize)) -> bool {
        // 複数選んだコミットの上なら、選びを保ったまま、選んだもの全部へのメニュー
        if self.log_marked_at(row, col, w) {
            return self.log_context_picked(at);
        }
        // 押す前の割り付けで当てる（押すとフォルダが畳まれて行が変わる）
        let hit = self.log_hit(row, col, w);
        self.log_click(row, col, w, 1);
        let Some(l) = &self.panels.log else { return false };
        let cur = self.svc.git.status.as_ref().map(|s| s.branch.clone()).unwrap_or_default();
        let upstream = self.svc.git.status.as_ref().and_then(|s| s.upstream.clone());
        // 右の列（選んだコミットの変えたファイル）
        let menu = if l.focus_files && matches!(hit, Some(LogHit::Detail(_) | LogHit::DetailArea)) {
            let Some(d) = &l.detail else { return false };
            let Some((_, path)) = d.files.get(l.file_sel).cloned() else { return false };
            let (h, short) = (d.hash.clone(), d.hash[..7.min(d.hash.len())].to_string());
            let mut m = crate::menu::Menu::new(at.0, at.1);
            m = m.item("差分の表示", |e| e.log_open_file_diff());
            let (x, p) = (h.clone(), path.clone());
            m = m.item("ローカルと比較", move |e| e.log_compare_worktree(&x, &p));
            let p = path.clone();
            m = m.item("ソースに移動", move |e| e.log_open_worktree_file(&p));
            let p = path.clone();
            m = m.item("履歴の表示", move |e| e.open_log_file(&p));
            m = m.sep();
            let (x, p, s) = (h.clone(), path.clone(), short.clone());
            m = m.item("このリビジョンから取得…", move |e| {
                let q = format!("{p} を {s} の版に戻しますか（作業中の変更は消える）");
                e.ask(q, move |e| {
                    e.git_guarded_args(&["checkout", &x, "--", &p], format!("{p} を {s} の版に戻しました"), |e, ok| {
                        if ok {
                            e.reload_log();
                        }
                    });
                })
            });
            let p = path.clone();
            m = m.item("パスをコピー", move |e| e.copy_text(p, "パス"));
            m
        } else if matches!(hit, Some(LogHit::Branch(_) | LogHit::BranchArea)) {
            let Some(i) = l.branch_sel.filter(|_| matches!(hit, Some(LogHit::Branch(BranchRow::Leaf { .. })))) else { return false };
            let (sec, name, is_cur) = l.branches[i].clone();
            let local = sec == "ローカル";
            let mut m = crate::menu::Menu::new(at.0, at.1);
            if !is_cur {
                m = m.item("チェックアウト", move |e| e.log_checkout_branch(i));
            }
            let n = name.clone();
            m = m.item(format!("{name} から新規ブランチ…"), move |e| e.prompt_command("Gbranch ", &format!(" {n}")));
            if !is_cur && sec != "タグ" {
                let (n, c) = (name.clone(), cur.clone());
                m = m.item(format!("チェックアウトして {cur} でリベース"), move |e| e.log_ask(Confirm::CheckoutRebase(n, c)));
                let n = name.clone();
                let c = cur.clone();
                m = m.item(format!("{cur} と比較"), move |e| e.open_log_range(&format!("{c}..{n}")));
                let n = name.clone();
                m = m.item(format!("{name} で {cur} をリベース"), move |e| e.log_ask(Confirm::Rebase(n)));
                let n = name.clone();
                m = m.item(format!("{name} を {cur} にマージ"), move |e| e.git_guarded_args(&["merge", "--no-edit", &n], format!("{n} をマージしました"), |e, _| e.reload_log()));
            }
            if local {
                m = m.sep();
                let n = name.clone();
                m = m.item("名前の変更…", move |e| e.prompt_command(&format!("Grename {n} "), ""));
                let n = name.clone();
                m = m.item("プッシュ…", move |e| e.run_git_job("push ", &["push", "-u", "origin", &n]));
                if !is_cur {
                    let n = name.clone();
                    m = m.item("削除", move |e| e.log_ask(Confirm::DeleteBranch(n, false)));
                }
            }
            m
        } else {
            let Some(c) = l.current().filter(|_| !l.focus_files).cloned() else { return false };
            let head = l.rows.iter().filter_map(|r| r.commit.as_ref()).find(|c| c.refs.starts_with("HEAD")).map(|c| c.hash.clone());
            let is_head = head.as_deref() == Some(c.hash.as_str());
            let (h, short) = (c.hash.clone(), c.short.clone());
            let mut m = crate::menu::Menu::new(at.0, at.1);
            // IntelliJ の Git の履歴と同じ並び: コピー → cherry-pick・チェックアウト → リセット・revert → 書き換え → プッシュ → 新規ブランチ・タグ → 親へ
            m = m.action(self, "log.copyHash").action(self, "log.copyHashSubject").sep().action(self, "log.cherryPick").action(self, "log.checkout");
            m = m.sep();
            let x = h.clone();
            m = m.item(format!("現在のブランチをここにリセット（--soft）"), move |e| e.log_ask(Confirm::Reset(x, "--soft")));
            let x = h.clone();
            m = m.item(format!("現在のブランチをここにリセット（--mixed）"), move |e| e.log_ask(Confirm::Reset(x, "--mixed")));
            let x = h.clone();
            m = m.item(format!("現在のブランチをここにリセット（--hard）"), move |e| e.log_ask(Confirm::Reset(x, "--hard")));
            let x = h.clone();
            m = m.item(format!("現在のブランチをここにリセット（--keep）"), move |e| e.log_ask(Confirm::Reset(x, "--keep")));
            let x = h.clone();
            m = m.item("コミットを revert", move |e| e.log_ask(Confirm::Revert(x)));
            if is_head {
                let x = h.clone();
                m = m.item("コミットの取り消し…", move |e| e.log_ask(Confirm::Undo(x)));
            }
            m = m.sep();
            let (x, subject) = (short.clone(), c.subject.clone());
            m = m.item("コミットメッセージの編集…", move |e| e.prompt_command(&format!("Greword {x} "), &subject));
            let x = h.clone();
            m = m.item("フィックスアップ…", move |e| e.log_ask(Confirm::Squash(x, false)));
            let x = h.clone();
            m = m.item("スカッシュイン…", move |e| e.log_ask(Confirm::Squash(x, true)));
            let x = h.clone();
            m = m.item("コミットの破棄", move |e| e.log_ask(Confirm::Drop(x)));
            let x = h.clone();
            m = m.item(format!("ここで {cur} をリベース"), move |e| e.log_ask(Confirm::Rebase(x)));
            if let Some(up) = upstream {
                let x = h.clone();
                m = m.item(format!("ここまでのすべてをプッシュ（{up}）…"), move |e| e.log_ask(Confirm::PushTo(x, up)));
            }
            m = m.sep().action(self, "log.newBranch");
            let x = short.clone();
            m = m.item("新規タグ…", move |e| e.prompt_command("Gtag ", &format!(" {x}")));
            m = m.sep();
            m = m.item("親コミットに移動", |e| e.log_goto_parent());
            m.sep().action(self, "log.filter").action(self, "log.hide").action(self, "log.close")
        };
        self.open_menu(menu);
        true
    }

    /// 真ん中の列の (row, col) が、複数選んだコミットの行か（そうなら、その行を今の行にする）
    fn log_marked_at(&mut self, row: usize, col: usize, w: usize) -> bool {
        let hit = self.log_hit(row, col, w);
        let Some(l) = &mut self.panels.log else { return false };
        if l.marked.len() < 2 {
            return false;
        }
        let hit = match hit {
            Some(LogHit::Commit(k)) => Some(k).filter(|&k| l.commit_at(k).is_some_and(|c| l.marked.contains(&c.hash))),
            _ => None,
        };
        if nagi_ui::list::pick(&mut l.selected, hit, 1, 2).is_none() {
            return false;
        }
        l.focus_files = false;
        l.filter = None;
        self.set_state(State::Log);
        true
    }

    /// 複数選んだコミットへのメニュー
    fn log_context_picked(&mut self, at: (usize, usize)) -> bool {
        let Some(l) = &self.panels.log else { return false };
        let picked = l.picked();
        let n = picked.len();
        let mut m = crate::menu::Menu::new(at.0, at.1);
        m = m.item(format!("{n} 件のリビジョン番号のコピー"), |e| e.log_copy_picked(false));
        m = m.item(format!("{n} 件のハッシュと題名をコピー"), |e| e.log_copy_picked(true));
        if n == 2 {
            let (new, old) = (picked[0].hash.clone(), picked[1].hash.clone());
            m = m.item("バージョンの比較", move |e| e.log_compare_two(&old, &new));
        }
        m = m.sep();
        m = m.item(format!("{n} 件を cherry-pick"), |e| e.log_cherry_pick_picked());
        let hs: Vec<String> = picked.iter().map(|c| c.hash.clone()).collect();
        m = m.item(format!("{n} 件のコミットを revert"), move |e| e.log_ask(Confirm::RevertMany(hs)));
        m = m.sep();
        m = m.item("選びを外す", |e| {
            if let Some(l) = &mut e.panels.log {
                l.marked.clear();
                l.mark_anchor = None;
            }
        });
        self.open_menu(m);
        true
    }

    /// 選んでいるコミットをチェリーピックする（複数なら古い順に）
    fn log_cherry_pick_picked(&mut self) {
        let Some(l) = &self.panels.log else { return };
        let mut hs: Vec<String> = l.picked().into_iter().map(|c| c.hash).collect();
        hs.reverse();
        match hs.len() {
            0 => {}
            1 => self.log_ask(Confirm::CherryPick(hs.remove(0))),
            _ => self.log_ask(Confirm::CherryPickMany(hs)),
        }
    }

    /// 選んでいるコミットのハッシュ（`subject` なら短いハッシュと題名）を、1 行に 1 つずつコピーする
    fn log_copy_picked(&mut self, subject: bool) {
        let Some(l) = &self.panels.log else { return };
        let picked = l.picked();
        if picked.is_empty() {
            return;
        }
        let text: Vec<String> = picked.iter().map(|c| if subject { format!("{} {}", c.short, c.subject) } else { c.hash.clone() }).collect();
        let what = match (subject, picked.len()) {
            (false, 1) => "ハッシュ".to_string(),
            (true, 1) => "ハッシュと題名".to_string(),
            (false, n) => format!("{n} 件のハッシュ"),
            (true, n) => format!("{n} 件のハッシュと題名"),
        };
        self.copy_text(text.join("\n"), &what);
    }

    /// 2 つのコミットを比べる: 右の列に、`old` から `new` までに変わったファイルを出す（Enter・ダブルクリックでその差分）
    fn log_compare_two(&mut self, old: &str, new: &str) {
        let waker = self.waker.clone();
        let Some(root) = self.svc.git.root.clone() else { return };
        let Some(l) = &mut self.panels.log else { return };
        // 右の列は今の行のコミットの中身なので、新しい方を今の行に
        if let Some(k) = l.visible.iter().position(|&i| l.rows[i].commit.as_ref().is_some_and(|c| c.hash == new)) {
            l.selected = k;
        }
        l.detail = None;
        l.focus_files = false;
        let (old, new) = (old.to_string(), new.to_string());
        l.rx_detail = spawn(waker, move || {
            let out = nagi_workers::git::git(&root, &["diff", "--name-status", &old, &new])?;
            let s = |h: &str| h[..7.min(h.len())].to_string();
            let head = format!("{new}\x1f\x1f\x1f{old}\x1f{} から {} までに変わったファイル\x1e\n", s(&old), s(&new));
            Ok(Detail { base: Some(old.clone()), ..parse_detail(&format!("{head}{}", String::from_utf8_lossy(&out))) })
        });
    }

    /// 選んでいるコミットの（1 つ目の）親の行へ
    fn log_goto_parent(&mut self) {
        let rows = self.log_rows();
        let Some(l) = &mut self.panels.log else { return };
        let Some(p) = l.detail.as_ref().and_then(|d| d.parents.split(' ').next().map(String::from)).filter(|p| !p.is_empty()) else { return };
        if let Some(k) = l.visible.iter().position(|&i| l.rows[i].commit.as_ref().is_some_and(|c| c.hash == p)) {
            l.selected = k;
            nagi_ui::list::fit(k, &mut l.top, rows);
        }
    }

    /// `:Greword ハッシュ 新しいメッセージ`
    pub(crate) fn git_reword(&mut self, arg: &str) {
        let Some((h, msg)) = arg.trim().split_once(' ') else {
            self.svc.notify.info("ハッシュと新しいメッセージを書いてください");
            return;
        };
        let (h, msg) = (h.to_string(), msg.trim().to_string());
        self.git_guarded(move |root| nagi_workers::git::reword(root, &h, &msg).map(|_| "メッセージを直しました".into()), |e, _| e.reload_log());
    }

    /// `:Gtag 名前 [ハッシュ]`
    pub(crate) fn git_tag(&mut self, arg: &str) {
        let mut it = arg.split_whitespace();
        let Some(name) = it.next() else {
            self.svc.notify.info("タグの名前を書いてください");
            return;
        };
        let mut args = vec!["tag", name];
        args.extend(it.next());
        self.git_guarded_args(&args, format!("タグ {name} を付けました"), |e, ok| {
            if ok {
                e.reload_log();
            }
        });
    }

    /// `:Grename 今の名前 新しい名前`
    pub(crate) fn git_rename_branch(&mut self, arg: &str) {
        let v: Vec<&str> = arg.split_whitespace().collect();
        let [old, new] = v[..] else {
            self.svc.notify.info("今の名前と新しい名前を書いてください");
            return;
        };
        self.git_guarded_args(&["branch", "-m", old, new], format!("{old} を {new} に変えました"), |e, ok| {
            if ok {
                e.reload_log();
            }
        });
    }

    pub(crate) fn close_log(&mut self) {
        self.panels.log = None;
        if self.state() == State::Log {
            self.set_state(State::Normal);
        }
    }

    /// 選んだコミットの、選んだファイルの差分（1 つ前の版 ↔ このコミット）
    fn log_open_file_diff(&mut self) {
        let (Some(root), Some(l)) = (self.svc.git.root.clone(), &self.panels.log) else { return };
        let Some(d) = &l.detail else { return };
        let Some((_, path)) = d.files.get(l.file_sel).cloned() else { return };
        let h = d.hash.clone();
        // 2 つのコミットを比べているときは古い方と、でなければ（1 つ目の）親と
        let parent = d.base.clone().or_else(|| d.parents.split(' ').next().filter(|p| !p.is_empty()).map(String::from));
        let short = h[..7.min(h.len())].to_string();
        let left = match &d.base {
            Some(b) => b[..7.min(b.len())].to_string(),
            None => format!("{short}^"),
        };
        // 2 つの版の中身は裏で読む（ADR 0018 の §4）
        let p = path.clone();
        let read = move || {
            let old = parent.as_deref().and_then(|x| nagi_workers::git::blob_at(&root, x, &p)).unwrap_or_default();
            (old, nagi_workers::git::blob_at(&root, &h, &p).unwrap_or_default())
        };
        self.job("git-blob", read, move |e, (old, new)| {
            e.show_diff(format!("{left}  {path}"), format!("{short}  {path}"), &old, &new, None);
            e.diff_return = Some(State::Log);
        });
    }

    /// コミット `h` の版の `path` と、作業中のファイルを比べる（右が作業中）
    fn log_compare_worktree(&mut self, h: &str, path: &str) {
        let Some(root) = self.svc.git.root.clone() else { return };
        let (h, path) = (h.to_string(), path.to_string());
        let short = h[..7.min(h.len())].to_string();
        let p = path.clone();
        let read = move || (nagi_workers::git::blob_at(&root, &h, &p).unwrap_or_default(), std::fs::read_to_string(root.join(&p)).unwrap_or_default());
        self.job("git-blob", read, move |e, (old, new)| {
            e.show_diff(format!("{short}  {path}"), format!("作業中  {path}"), &old, &new, None);
            e.diff_return = Some(State::Log);
        });
    }

    /// 作業ツリーの `path` を開く（無ければ知らせる）
    fn log_open_worktree_file(&mut self, path: &str) {
        let Some(root) = self.svc.git.root.clone() else { return };
        let p = root.join(path);
        if !p.exists() {
            self.svc.notify.warn(format!("{path} は作業ツリーにありません"));
            return;
        }
        self.edit_file(p);
        self.set_state(State::Normal);
    }

    /// `path`（根からの / 区切り）の履歴を開く
    fn open_log_file(&mut self, path: &str) {
        let Some(root) = self.svc.git.root.clone() else { return };
        self.panels.log = None;
        self.load_log(root, Some(path.to_string()), None);
    }

    /// `:Gbranch 名前 [起点]`: ブランチを作ってチェックアウトする
    pub(crate) fn git_new_branch(&mut self, arg: &str) {
        let mut it = arg.split_whitespace();
        let Some(name) = it.next() else {
            self.svc.notify.info("ブランチの名前を書いてください");
            return;
        };
        let mut args = vec!["checkout", "-q", "-b", name];
        if let Some(start) = it.next() {
            args.push(start);
        }
        // 履歴を出していれば読み直す（新しいブランチが見える）
        self.git_guarded_args(&args, format!("ブランチ {name} を作ってチェックアウトしました"), |e, _| e.reload_log());
    }
}



/// この部品の入口（component.rs）
pub struct LogComponent;

impl crate::component::Component for LogComponent {
    fn status(&self, ed: &crate::editor::Editor) -> Option<crate::component::Status> {
        let name = match ed.panels.log.as_ref().filter(|l| l.filter.is_some()) {
            Some(l) => format!("/{}", l.filter_text),
            None => "-- 履歴 --".into(),
        };
        crate::component::Status::new(name, "↑↓ 選ぶ  Tab ファイル  ダブルクリック ファイル  右クリック 操作  打つと絞り込み  Esc 窓へ ")
    }

    fn key(&self, ed: &mut crate::editor::Editor, key: nagi_vim::Key) -> crate::editor::Effect {
        ed.log_key(key);
        crate::editor::Effect::None
    }

    fn scope(&self, ed: &crate::editor::Editor) -> Option<crate::ui_action::Scope> {
        let _ = ed;
        Some(crate::ui_action::Scope::Log)
    }

    /// 絞り込みの欄に打っている間の文字
    fn text(&self, ed: &mut crate::editor::Editor, text: &str) {
        if let Some(l) = &mut ed.panels.log {
            if let Some(f) = &mut l.filter {
                f.insert_text(text);
                l.filter_text = f.text();
                l.refilter_pub();
            }
        }
    }
}


/// 履歴の画面（layout.rs の View）
pub struct LogScreenView;

impl crate::layout::View for LogScreenView {
    fn mono(&self) -> bool {
        true
    }

    fn rect(&self, ed: &crate::editor::Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let _ = s;
        if ed.panels.diff.is_some() || !ed.panels.log.as_ref().is_some_and(|l| !l.docked) {
            return None;
        }
        let (x, w) = ed.windows_x();
        Some(crate::panes::Rect { x, y: ed.area().y, w, h: ed.windows_rows().saturating_sub(ed.area().y) })
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        if let Some(l) = &ed.panels.log {
            draw::draw_log_view(p, l, r, ed.log_cols_for(r.w), ed.log_right_cols(), &ed.preedit, ed.svc.git.status.as_ref().map(|s| (s.branch.as_str(), s.ahead as usize, s.behind as usize)));
        }
    }

    fn click(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, at: crate::layout::At, clicks: u32) -> crate::layout::Click {
        // 窓を覆って出ている履歴: 下の欄と同じ割り付けで選ぶ
        ed.log_click(at.row, at.col, r.w, clicks);
        crate::layout::Click::Done
    }

    fn context(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, at: crate::layout::At) -> bool {
        let (row, col) = at.screen;
        ed.log_context(at.row, at.col, r.w, (row, col))
    }

    fn wheel(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, at: crate::layout::At, dy: isize, dx: isize) -> bool {
        ed.log_wheel(dy, dx, r.w, at.col);
        true
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// 履歴（窓の領域全体）。左にブランチ、中央にグラフ付きのコミット、右に選んだコミットの中身
    /// `(aw, tw, hw)` はコミットの作者・日時・ハッシュの列の幅、`ab` は今のブランチの (名前, 送るコミット数, 受け取るコミット数)
    pub(crate) fn draw_log_view(p: &mut Painter, l: &crate::log_view::LogView, r: Rect, (bw, dw): (usize, usize), (aw, tw, hw): (usize, usize, usize), preedit: &str, ab: Option<(&str, usize, usize)>) {
        const LANES: [[f32; 4]; 5] = [rgb(0x87afd7), rgb(0xd7af87), rgb(0x87af87), rgb(0xd787af), rgb(0xafafd7)];
        // 複数選んだコミットの行
        const MARKED_BG: [f32; 4] = rgb(0x264f78);
        let cell = p.fonts.cell();
        let (x0, y0) = (r.x as f32 * cell.w, r.y as f32 * cell.h);
        p.out.push(Instance::fill(x0, y0, r.w as f32 * cell.w, r.h as f32 * cell.h, BG));
        let cx = r.x + bw + 1;
        let cw = r.w.saturating_sub(bw + dw + 2);
        let dx = cx + cw + 1;
        // 上の行: 絞り込みと件数
        let n = l.visible.iter().filter(|&&i| l.rows[i].commit.is_some()).count();
        let tail = format!(
            "{} 件{}{}{}",
            n,
            l.path.as_ref().map_or(String::new(), |p| format!("   ファイル: {p}")),
            if l.marked.is_empty() { String::new() } else { format!("   {} 件を選択（右クリックで操作・Esc で外す）", l.marked.len()) },
            if l.loading { " …" } else { "" }
        );
        // 左: 絞り込みの字（欄の幅まで）、その右にブランチ・ユーザー・日付の札、件数
        let saved_cols = p.cols;
        p.cols = r.x + crate::log_view::LOG_FILTER_W - 1;
        if let Some(f) = &l.filter {
            let c = p.text(" /", r.x, y0, FG);
            draw_vim_field(p, f, c, y0, preedit);
        } else {
            p.text(&format!(" {}", if l.filter_text.is_empty() { "打つと絞り込み".to_string() } else { format!("/{}", l.filter_text) }), r.x, y0, STATUS_FG);
        }
        p.cols = saved_cols;
        let mut end = r.x + crate::log_view::LOG_FILTER_W;
        for (label, x, w, c) in l.chips() {
            let on = match c {
                crate::log_view::LogChip::Branch => l.query.branch.is_some(),
                crate::log_view::LogChip::User => l.query.user.is_some(),
                crate::log_view::LogChip::Date => l.query.period.is_some(),
            };
            p.text(&label, r.x + x, y0, if on { FG_BRIGHT } else { STATUS_FG });
            end = r.x + x + w + 2;
        }
        // 右上のブランチの数と重ねない
        let abw = ab.map_or(0, |(name, ahead, behind)| p.width(&format!("{name}  ↑{ahead} 送る  ↓{behind} 受け取る ")) + 1);
        p.cols = (r.x + r.w).saturating_sub(abw);
        p.text(&format!(" {tail}"), end, y0, STATUS_FG);
        p.cols = saved_cols;
        // 右上: 今のブランチと、送る（↑）・受け取る（↓）コミットの数（受け取る数は最後にフェッチした時点）
        if let Some((name, ahead, behind)) = ab {
            let s = format!("{name}  ↑{ahead} 送る  ↓{behind} 受け取る ");
            let x = (r.x + r.w).saturating_sub(p.width(&s));
            let c = p.text(&format!("{name}  "), x, y0, FG);
            let c = p.text(&format!("↑{ahead}"), c, y0, if ahead > 0 { GIT_ADD } else { STATUS_FG });
            let c = p.text(" 送る  ", c, y0, STATUS_FG);
            let c = p.text(&format!("↓{behind}"), c, y0, if behind > 0 { DIAG_WARN } else { STATUS_FG });
            p.text(" 受け取る ", c, y0, STATUS_FG);
        }
        p.out.push(Instance::fill(x0, y0 + cell.h - 1.0, r.w as f32 * cell.w, 1.0, LINE_NO));
        let body = r.h.saturating_sub(1);
        // 左: ブランチ
        let saved = p.cols;
        p.cols = r.x + bw;
        // 割り付け（LogView::ui。押した位置もこれで当てる）の行に描く
        let ui = l.ui(r.w, r.h, (bw, dw));
        let y_of = |rect: nagi_ui::Rect| (r.y + rect.y) as f32 * cell.h;
        // 区分（ローカル・リモート・タグ）・フォルダとブランチ。字下げ 1 段と ▸ ▾ の幅はアイコンの幅
        let ind = crate::metrics::icon_cols(crate::metrics::Icon::Small).max(2);
        let px = crate::metrics::icon(crate::metrics::Icon::Small);
        for (rect, hit) in ui.iter() {
            let LogHit::Branch(item) = hit else { continue };
            let y = y_of(rect);
            match item {
                BranchRow::Folder { key, name, depth } => {
                    let icon = if !l.branch_folds.is_open(key) { "g_chevronRight" } else { "g_chevronDown" };
                    let x = r.x + depth * ind;
                    crate::icons::draw(p, icon, x as f32 * cell.w, crate::metrics::centered(y, px), px, None);
                    p.text(name, x + ind, y, if *depth == 0 { STATUS_FG } else { FG });
                }
                BranchRow::Leaf { i, depth } => {
                    let (_, full, cur) = &l.branches[*i];
                    let name = full.rsplit('/').next().unwrap_or(full);
                    if l.branch_sel == Some(*i) {
                        p.out.push(Instance::fill(x0, y, bw as f32 * cell.w, cell.h, MATCH_BG));
                    }
                    // 今のブランチは * を字下げの所に
                    let x = r.x + depth * ind;
                    if *cur {
                        p.text("*", x + ind.saturating_sub(2), y, FG);
                    }
                    p.text(name, x + ind, y, if *cur { FG } else { STATUS_FG });
                }
            }
        }
        p.cols = saved;
        p.out.push(Instance::fill((r.x + bw) as f32 * cell.w + cell.w * 0.5, y0 + cell.h, 1.0, body as f32 * cell.h, LINE_NO));
        // 中央: コミット
        let saved = p.cols;
        p.cols = cx + cw;
        for (rect, hit) in ui.iter() {
            let (LogHit::Commit(k) | LogHit::Graph(k)) = *hit else { continue };
            let row = &l.rows[l.visible[k]];
            let y = y_of(rect);
            let sel = k == l.selected;
            let focused = sel && !l.focus_files;
            let marked = row.commit.as_ref().is_some_and(|c| l.marked.contains(&c.hash));
            if focused {
                selection(p, cx as f32 * cell.w, y, cw as f32 * cell.w, cell.h, LIST_SEL_BG);
            } else if marked {
                p.out.push(Instance::fill(cx as f32 * cell.w, y, cw as f32 * cell.w, cell.h, MARKED_BG));
            } else if sel {
                p.out.push(Instance::fill(cx as f32 * cell.w, y, cw as f32 * cell.w, cell.h, MATCH_BG));
            }
            // グラフ・ref・題名を 1 つに並べ、横に送った分を落としてから描く
            let graph = crate::log_view::pretty_graph(&row.graph);
            let mut items: Vec<Item> = Vec::new();
            for (j, (_, g)) in graph.grapheme_indices(true).enumerate() {
                let color = if focused { FG_BRIGHT } else { LANES[(j / 2) % LANES.len()] };
                items.push(Item { g: Cow::Owned(g.to_string()), width: p.cell_width(g), fg: color, bg: None, underline: false });
            }
            let Some(cm) = &row.commit else {
                skip_cols(&mut items, l.left, &mut None);
                p.draw(&items, cx + 1, y);
                continue;
            };
            {
                let (fg, dim, refc) = if focused { (FG_BRIGHT, FG_BRIGHT, FG_BRIGHT) } else { (FG, STATUS_FG, rgb(0xd7af87)) };
                // 作者と日時は表示の幅で揃える（全角は 2 セル）
                let pad = |p: &Painter, s: &str, w: usize| {
                    let t: String = s.chars().scan(0, |acc, ch| {
                        *acc += p.width(ch.encode_utf8(&mut [0u8; 4]));
                        (*acc <= w).then_some(ch)
                    }).collect();
                    let tw = p.width(&t);
                    format!("{t}{}", " ".repeat(w.saturating_sub(tw)))
                };
                // 作者・日時・ハッシュの列（幅は log_right_cols。境目は log_commit_seps でつかむ）
                let right = format!("{} {} {}", pad(p, &cm.author, aw), pad(p, &cm.date, tw), pad(p, &cm.short, hw));
                let rw = aw + tw + hw + 2;
                items.extend(p.items(" ", fg).into_iter().map(|i| Item { g: Cow::Owned(i.g.into_owned()), ..i }));
                if !cm.refs.is_empty() {
                    items.extend(p.items(&format!("{}  ", cm.refs), refc).into_iter().map(|i| Item { g: Cow::Owned(i.g.into_owned()), ..i }));
                }
                items.extend(p.items(&cm.subject, fg));
                let limit = (cx + cw).saturating_sub(rw + 1);
                skip_cols(&mut items, l.left, &mut None);
                let saved2 = p.cols;
                p.cols = limit.saturating_sub(1);
                p.draw(&items, cx + 1, y);
                p.cols = saved2;
                p.text(&right, limit, y, dim);
            }
        }
        p.cols = saved;
        p.out.push(Instance::fill((cx + cw) as f32 * cell.w + cell.w * 0.5, y0 + cell.h, 1.0, body as f32 * cell.h, LINE_NO));
        // 右: 選んだコミットの中身
        let saved = p.cols;
        p.cols = r.x + r.w;
        if let Some(d) = &l.detail {
            // 中身の行（detail_lines を detail_top から）。メッセージは折り返し、ファイルは detail_left だけ横に送る
            for (rect, hit) in ui.iter() {
                let LogHit::Detail(line) = hit else { continue };
                let y = y_of(rect);
                match line {
                    DetailLine::Info(t) => {
                        p.text(t, dx + 1, y, STATUS_FG);
                    }
                    DetailLine::Message(t, first) => {
                        p.text(t, dx + 1, y, if *first { rgb(0xe8e8e8) } else { STATUS_FG });
                    }
                    DetailLine::Gap => {}
                    DetailLine::FilesHead(n) => {
                        p.text(&format!("変えたファイル（{n}）"), dx + 1, y, STATUS_FG);
                    }
                    DetailLine::File(f) => {
                        let (st, path) = &d.files[*f];
                        let sel = l.focus_files && *f == l.file_sel;
                        if sel {
                            selection(p, dx as f32 * cell.w, y, (r.x + r.w - dx) as f32 * cell.w, cell.h, LIST_SEL_BG);
                        }
                        let sc = if sel {
                            FG_BRIGHT
                        } else {
                            match st {
                                'A' => GIT_ADD,
                                'D' => GIT_DEL,
                                'M' => GIT_MOD,
                                _ => rgb(0xd7af5f),
                            }
                        };
                        let (name, dir) = crate::log_view::file_line(*st, path);
                        let mut items = p.items(&name, if sel { FG_BRIGHT } else { FG });
                        if let Some(first) = items.first_mut() {
                            first.fg = sc;
                        }
                        items.extend(p.items(&dir, if sel { FG_BRIGHT } else { STATUS_FG }));
                        skip_cols(&mut items, l.detail_left, &mut None);
                        p.draw(&items, dx + 1, y);
                    }
                }
            }
        } else if !l.rows.is_empty() {
            p.text("…", dx + 1, y0 + cell.h, STATUS_FG);
        }
        p.cols = saved;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse() {
        let out = "*\x1fa1b2c3d\x1fHEAD -> main\x1f注文を直す\x1f自分\x1f3 days ago\x1fa1b2c3d4\n|\\  \n| *\x1f7e6f5a4\x1f\x1fクーポン\x1fB\x1f4 days ago\x1f7e6f5a4b\n";
        let rows = parse_log(out);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].commit.as_ref().unwrap().refs, "HEAD -> main");
        assert!(rows[1].commit.is_none());
        assert_eq!(pretty_graph(&rows[2].graph), "│ ●");
        let d = parse_detail("abc\x1f自分\x1f3 days ago\x1fp1 p2\x1f件名\n\n本文\n\x1e\n\nM\tsrc/a.rs\nR100\told.rs\tnew.rs\n");
        assert_eq!((d.hash.as_str(), d.parents.as_str(), d.message.as_str()), ("abc", "p1 p2", "件名\n\n本文"));
        assert_eq!(d.files, vec![('M', "src/a.rs".to_string()), ('R', "new.rs".to_string())]);
    }

    /// 部品のキーは Editor を作らずに確かめられる（部品の中で済むことは行い、外に及ぶことは Action で返す）
    #[test]
    fn keys_without_editor() {
        let mut svc = crate::editor::Services::default();
        let mut l = view("*\x1fa1\x1f\x1f一\x1fA\x1f1\x1fa1\n|\n*\x1fb2\x1f\x1f二\x1fA\x1f2\x1fb2\n", &[], Some(parse_detail("a1\x1fA\x1f1\x1fp\x1f件名\x1e\n\nM\ta.rs\nA\tb.rs\n")));
        assert_eq!(l.key(Key::Down, 10, &mut svc), LogAction::None);
        assert_eq!(l.selected, 2, "グラフだけの行は飛ばす");
        assert_eq!(l.key(Key::Tab, 10, &mut svc), LogAction::None);
        assert!(l.focus_files);
        assert_eq!(l.key(Key::Down, 10, &mut svc), LogAction::FitDetail);
        assert_eq!((l.file_sel, l.key(Key::Enter, 10, &mut svc)), (1, LogAction::OpenFileDiff));
        assert_eq!(l.key(Key::Esc, 10, &mut svc), LogAction::None);
        assert_eq!(l.key(Key::Esc, 10, &mut svc), LogAction::ToText, "下の欄に収めているときの Esc は本文へ");
        // 絞り込みの欄も部品の中で（打った字から絞り込む。Enter で決めて欄を閉じる。1 文字キーの操作は無い: ADR 0019）
        l.key(Key::Char('二'), 10, &mut svc);
        assert!(l.filter.is_some());
        assert_eq!(l.filter_text, "二");
        assert_eq!(l.visible.iter().filter(|&&i| l.rows[i].commit.is_some()).count(), 1);
        l.key(Key::Enter, 10, &mut svc);
        assert!(l.filter.is_none());
    }

    /// 試験用の履歴（下の欄に収めて出す）
    fn view(log: &str, branches: &[(&str, &str, bool)], detail: Option<Detail>) -> LogView {
        let rows = parse_log(log);
        LogView {
            left: 0,
            branch_sel: None,
            branch_top: 0,
            branch_folds: nagi_ui::Folds::open(),
            detail_top: 0,
            detail_left: 0,
            auto_cols: auto_cols(&rows),
            want_hash: None,
            detail_cache: Default::default(),
            src_path: None,
            src_range: None,
            keep_view: false,
            query: LogQuery::default(),
            limit: LOG_PAGE,
            docked: true,
            visible: (0..rows.len()).collect(),
            rows,
            selected: 0,
            marked: Vec::new(),
            mark_anchor: None,
            top: 0,
            branches: branches.iter().map(|(a, b, c)| (a.to_string(), b.to_string(), *c)).collect(),
            detail,
            focus_files: false,
            file_sel: 0,
            filter_text: String::new(),
            filter: None,
            path: None,
            loading: false,
            rx_log: None,
            rx_branches: None,
            rx_detail: None,
        }
    }

    /// ブランチの列: 名前の / でフォルダに分け（リモートは origin の下）、区分・フォルダを押すと畳む・開く
    #[test]
    fn branch_folders() {
        let mut l = view("", &[("ローカル", "main", true), ("ローカル", "feature/a", false), ("ローカル", "feature/b/c", false), ("リモート", "origin/main", false), ("リモート", "origin/feature/a", false)], None);
        let show = |l: &LogView| {
            l.branch_rows()
                .iter()
                .map(|r| match r {
                    BranchRow::Folder { name, depth, .. } => format!("{}{name}/", " ".repeat(*depth)),
                    BranchRow::Leaf { i, depth } => format!("{}{}", " ".repeat(*depth), l.branches[*i].1.rsplit('/').next().unwrap()),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(show(&l), ["ローカル/", " feature/", "  b/", "   c", "  a", " main", "リモート/", " origin/", "  feature/", "   a", "  main"]);
        l.toggle_branch_folder("ローカル/feature");
        assert_eq!(show(&l)[..3], ["ローカル/", " feature/", " main"]);
        assert_eq!(l.branch_at(2), Some(0), "畳んだ後もブランチの番号で引ける");
        l.toggle_branch_folder("リモート");
        assert_eq!(show(&l).len(), 4);
        // 押して畳む（下の欄: 0 行目はタブ、1 行目は絞り込みの見出し、2 行目から木）
        let mut e = Editor::new(nagi_core::Buffer::from_str("x"), None);
        e.set_screen(120, 40);
        e.panels.log = Some(l);
        e.select_bottom_tab(crate::result_grid::BottomKind::Git);
        e.bottom_click(2, 2, 1);
        assert_eq!(show(e.panels.log.as_ref().unwrap()), ["ローカル/", "リモート/"]);
        e.bottom_click(2, 2, 1);
        assert_eq!(show(e.panels.log.as_ref().unwrap()), ["ローカル/", " feature/", " main", "リモート/"], "開くと、中で畳んだフォルダは畳んだまま");
        e.bottom_click(3, 2, 1);
        assert_eq!(show(e.panels.log.as_ref().unwrap())[..4], ["ローカル/", " feature/", "  b/", "   c"]);
    }

    /// コミットの作者・日時の列は中身に合わせた幅で、境目をつかんで変えられる
    #[test]
    fn commit_columns() {
        let mut e = Editor::new(nagi_core::Buffer::from_str("x"), None);
        e.set_screen(160, 40);
        e.panels.log = Some(view("*\x1fa1\x1f\x1f一\x1fとても長い名前の作者\x1f12 minutes ago\x1fa1b2c3d4\n", &[], None));
        e.select_bottom_tab(crate::result_grid::BottomKind::Git);
        assert_eq!(e.log_right_cols(), (16, 14, 7), "作者は 16 列まで、日時は中身に合わせる（ハッシュは 7 列から）");
        let r = e.log_rect().unwrap();
        let (_, dw) = e.log_cols_for(r.w);
        let end = r.x + r.w - dw - 1;
        let (a, d) = e.log_commit_seps(end);
        let sep = e.sep_at(r.y + 2, a).expect("題名と作者の境目");
        assert!(sep.horizontal_drag());
        e.drag_sep(&sep, r.y + 2, a - 4);
        assert_eq!(e.log_right_cols().0, 20, "左へ 4 列で作者が 4 列広がる");
        let sep = e.sep_at(r.y + 2, d).expect("作者と日時の境目");
        e.drag_sep(&sep, r.y + 2, d + 4);
        assert_eq!(e.log_right_cols().1, 10, "右へ 4 列で日時が 4 列狭まる");
        assert_eq!(e.log_commit_seps(end).1, d + 4);
        e.reset_sep(&sep);
        assert_eq!(e.log_right_cols().1, 14);
    }

    /// 右の中身: メッセージは幅で折り返して全部出し、中身全体をホイールで送れる。押した行のファイルを選ぶ
    #[test]
    fn detail_wraps_and_scrolls() {
        let msg: String = (0..30).map(|k| format!("行 {k} の説明はとても長くて一行に入りきらないので折り返して出します")).collect::<Vec<_>>().join("\n");
        let files: String = (0..40).map(|k| format!("M\tsrc/very/deep/folder/name/file{k}.rs\n")).collect();
        let d = parse_detail(&format!("a1\x1fA\x1f1\x1fp\x1f件名\n\n{msg}\x1e\n\n{files}"));
        let lines = detail_lines(&d, 20);
        assert!(lines.iter().filter(|l| matches!(l, DetailLine::Message(..))).count() > 60, "30 行を全部、折り返して出す");
        assert!(lines.iter().all(|l| match l {
            DetailLine::Message(t, _) | DetailLine::Info(t) => nagi_core::display_col(t, t.len(), false) <= 20,
            _ => true,
        }));
        assert_eq!(wrap("abc def ghi", 7), ["abc def", "ghi"]);
        let mut e = Editor::new(nagi_core::Buffer::from_str("x"), None);
        e.set_screen(120, 40);
        e.panels.log = Some(view("*\x1fa1\x1f\x1f一\x1fA\x1f1\x1fa1\n", &[], Some(d)));
        e.select_bottom_tab(crate::result_grid::BottomKind::Git);
        let r = e.log_rect().unwrap();
        let (_, dw) = e.log_cols_for(r.w);
        let n = detail_lines(e.panels.log.as_ref().unwrap().detail.as_ref().unwrap(), dw - 1).len();
        e.log_wheel(1000, 0, r.w, r.w - 2);
        let rows = e.log_rows();
        assert_eq!(e.panels.log.as_ref().unwrap().detail_top, n - rows, "最後の行まで送れる");
        // 送った後に押した行（最後の行）は、最後のファイル
        e.bottom_click(1 + rows, r.w - 5, 1);
        let l = e.panels.log.as_ref().unwrap();
        assert_eq!((l.focus_files, l.file_sel), (true, 39));
        // 横にも送れる（長いパス）
        e.log_wheel(0, 5, r.w, r.w - 2);
        assert!(e.panels.log.as_ref().unwrap().detail_left > 0);
        // キーで選びを上へ動かすと、見える範囲へ戻る
        e.log_wheel(-1000, 0, r.w, r.w - 2);
        e.set_state(State::Log);
        e.key(nagi_vim::Key::Up);
        let l = e.panels.log.as_ref().unwrap();
        let k = detail_lines(l.detail.as_ref().unwrap(), dw - 1).iter().position(|x| *x == DetailLine::File(38)).unwrap();
        assert!(l.detail_top <= k && k < l.detail_top + rows, "{} {k}", l.detail_top);
    }

    /// 本文へ移った後でも、Git の欄の行を押せば選び直せる
    #[test]
    fn click_selects_commit_and_file() {
        let mut e = Editor::new(nagi_core::Buffer::from_str("x"), None);
        e.set_screen(120, 40);
        let rows = parse_log("*\x1fa1\x1f\x1f一\x1fA\x1f1\x1fa1\n|\n*\x1fb2\x1f\x1f二\x1fA\x1f2\x1fb2\n");
        e.panels.log = Some(LogView {
            left: 0,
            branch_sel: None,
            branch_top: 0,
            branch_folds: nagi_ui::Folds::open(),
            detail_top: 0,
            detail_left: 0,
            auto_cols: (10, 10, 7),
            want_hash: None,
            detail_cache: Default::default(),
            src_path: None,
            src_range: None,
            keep_view: false,
            query: LogQuery::default(),
            limit: LOG_PAGE,
            docked: true,
            visible: (0..rows.len()).collect(),
            rows,
            selected: 0,
            marked: Vec::new(),
            mark_anchor: None,
            top: 0,
            branches: Vec::new(),
            detail: Some(parse_detail("a1\x1fA\x1f1\x1fp\x1f件名\x1e\n\nM\ta.rs\nA\tb.rs\n")),
            focus_files: false,
            file_sel: 0,
            filter_text: String::new(),
            filter: None,
            path: None,
            loading: false,
            rx_log: None,
            rx_branches: None,
            rx_detail: None,
        });
        e.select_bottom_tab(crate::result_grid::BottomKind::Git);
        e.set_state(State::Normal);
        // 真ん中の 3 行目（グラフだけの行の次）を押す。欄の 0 行目はタブ、1 行目は絞り込みの見出し
        e.bottom_click(4, 60, 1);
        assert_eq!(e.state(), State::Log);
        assert_eq!(e.panels.log.as_ref().unwrap().selected, 2);
        // グラフだけの行は選ばない
        e.bottom_click(3, 60, 1);
        assert_eq!(e.panels.log.as_ref().unwrap().selected, 2);
        // 本文へ戻ってから、右の変えたファイル（見出し 2 行・空き・件名 1 行・空き・「変えたファイル」の次の次）を押す
        e.set_state(State::Normal);
        e.bottom_click(1 + 1 + 7, 110, 1);
        let l = e.panels.log.as_ref().unwrap();
        assert_eq!((e.state(), l.focus_files, l.file_sel), (State::Log, true, 1));
        // 真ん中を押せばコミットの選びへ戻る
        e.bottom_click(2, 60, 1);
        let l = e.panels.log.as_ref().unwrap();
        assert_eq!((l.focus_files, l.selected), (false, 0));
    }

    /// 本物のリポジトリで: コミットを選ぶと右の中身がそのコミットになる（裏の仕事が無くても、描く前に取りに行く）・
    /// ブランチの列はホイールで送れる・列の間の線をドラッグで動かせる
    /// git log の引数: 絞り込み（ブランチは --all の代わり・ユーザー・期間）とファイルの履歴
    #[test]
    fn log_query_args() {
        let a = |path: Option<&str>, q: &LogQuery| log_args(path, None, q, 1000).join(" ");
        let d = LogQuery::default();
        assert!(a(None, &d).starts_with("log --graph --date-order -n 1000 "));
        assert!(a(None, &d).ends_with(" --all --"));
        let q = LogQuery { branch: Some("dev".into()), user: Some("a.b (x)".into()), period: Some(1) };
        let s = a(None, &q);
        assert!(s.contains(" --author=a\\.b \\(x\\)") && s.contains(" --since=7.days.ago") && s.ends_with(" dev --") && !s.contains("--all"), "{s}");
        assert!(a(Some("src/a.rs"), &d).starts_with("log --follow -n 1000 ") && a(Some("src/a.rs"), &d).ends_with(" -- src/a.rs"));
    }

    /// 札: ユーザーを選ぶとその人のコミットだけを読み直す。「すべて」で戻す
    #[test]
    fn chips_filter_by_user() {
        let dir = std::env::temp_dir().join(format!("nagi-logchips-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| String::from_utf8_lossy(&nagi_workers::git::git(&dir, args).unwrap()).trim().to_string();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "test@example.com"]);
        for (f, who) in [("a", "alice"), ("b", "bob"), ("c", "alice")] {
            std::fs::write(dir.join(f), format!("{f}\n")).unwrap();
            g(&["add", "."]);
            g(&["-c", &format!("user.name={who}"), "commit", "-q", "-m", &format!("add {f}")]);
        }
        let mut buf = nagi_core::Buffer::open(&dir.join("a")).unwrap();
        buf.path = Some(dir.join("a"));
        let mut e = Editor::new(buf, None);
        e.set_screen(160, 40);
        e.start_git();
        e.open_log(false);
        let wait = |e: &mut Editor, what: &dyn Fn(&Editor) -> bool| {
            for _ in 0..500 {
                e.poll_background();
                if what(e) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!("届かない");
        };
        let authors = |e: &Editor| e.panels.log.as_ref().unwrap().rows.iter().filter_map(|r| r.commit.as_ref()).map(|c| c.author.clone()).collect::<Vec<_>>();
        wait(&mut e, &|e| e.panels.log.as_ref().is_some_and(|l| !l.loading));
        assert_eq!(authors(&e).len(), 3);
        // 0 行目の「ユーザー」の札を押す → メニュー（すべて・多い順）
        let r = e.log_rect().unwrap();
        let chip = e.panels.log.as_ref().unwrap().chips().into_iter().find(|c| c.3 == LogChip::User).unwrap();
        e.log_click(0, chip.1 + 1, r.w, 1);
        let labels: Vec<String> = e.menu.as_ref().unwrap().items.iter().map(|i| i.label.trim().to_string()).collect();
        assert_eq!(labels.iter().filter(|l| !l.is_empty()).cloned().collect::<Vec<_>>(), ["✓ すべて", "alice", "bob"]);
        let k = e.menu.as_ref().unwrap().items.iter().position(|i| i.label.ends_with("bob")).unwrap();
        e.menu_run(k);
        wait(&mut e, &|e| e.panels.log.as_ref().is_some_and(|l| !l.loading));
        assert_eq!(authors(&e), ["bob"]);
        assert!(e.panels.log.as_ref().unwrap().chips()[1].0.contains("bob"));
        // 操作の後に読み直しても絞り込みは残る
        e.reload_log();
        wait(&mut e, &|e| e.panels.log.as_ref().is_some_and(|l| !l.loading));
        assert_eq!(authors(&e), ["bob"]);
        // 読んだ分の終わりに近づいたら続きを読む（数を倍に）
        let l = e.panels.log.as_mut().unwrap();
        l.query = LogQuery::default();
        l.limit = 2;
        e.log_requery(None, false);
        wait(&mut e, &|e| e.panels.log.as_ref().is_some_and(|l| !l.loading));
        assert_eq!(authors(&e).len(), 2);
        e.log_more_if_needed(30);
        assert_eq!(e.panels.log.as_ref().unwrap().limit, 4);
        wait(&mut e, &|e| e.panels.log.as_ref().is_some_and(|l| !l.loading));
        assert_eq!(authors(&e).len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detail_follows_and_columns() {
        let dir = std::env::temp_dir().join(format!("nagi-logcols-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| String::from_utf8_lossy(&nagi_workers::git::git(&dir, args).unwrap()).trim().to_string();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "test@example.com"]);
        g(&["config", "user.name", "test"]);
        for f in ["a", "b", "c"] {
            std::fs::write(dir.join(f), format!("{f}\n")).unwrap();
            g(&["add", "."]);
            g(&["commit", "-q", "-m", &format!("add {f}")]);
        }
        for k in 0..40 {
            g(&["branch", &format!("b{k:02}")]);
        }
        let mut buf = nagi_core::Buffer::open(&dir.join("a")).unwrap();
        buf.path = Some(dir.join("a"));
        let mut e = Editor::new(buf, None);
        e.set_screen(160, 40);
        e.start_git();
        e.open_log(false);
        let wait = |e: &mut Editor, what: &dyn Fn(&Editor) -> bool| {
            for _ in 0..500 {
                e.poll_background();
                if what(e) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!("届かない");
        };
        wait(&mut e, &|e| e.panels.log.as_ref().is_some_and(|l| !l.loading && !l.branches.is_empty() && l.detail.is_some()));
        // 2 つ目のコミットを押す → 描く前の取りに行き（view::build が呼ぶ）で、右の中身がそのコミットになる
        let r = e.log_rect().unwrap();
        let (bw, _) = e.log_cols_for(r.w);
        e.log_click(2, bw + 5, r.w, 1);
        let want = e.panels.log.as_ref().unwrap().current().unwrap().hash.clone();
        assert_ne!(e.panels.log.as_ref().unwrap().detail.as_ref().unwrap().hash, want);
        e.log_request_detail();
        assert!(e.panels.log.as_ref().unwrap().rx_detail.is_some(), "選んだらすぐ取りに行く");
        wait(&mut e, &|e| e.panels.log.as_ref().unwrap().detail.as_ref().is_some_and(|d| d.hash == want));
        // ブランチの列はホイールで送る（41 本で画面に入らない）。送った分だけ、押した行のブランチもずれる
        let first = e.panels.log.as_ref().unwrap().branch_at(0);
        e.log_wheel(5, 0, r.w, 1);
        let l = e.panels.log.as_ref().unwrap();
        assert_eq!(l.branch_top, 5);
        assert_ne!(l.branch_at(0), first);
        assert_eq!(l.branch_at(0), match &l.branch_rows()[5] { BranchRow::Leaf { i, .. } => Some(*i), _ => None });
        e.log_wheel(-100, 0, r.w, 1);
        assert_eq!(e.panels.log.as_ref().unwrap().branch_top, 0);
        // 列の間の線をつかんで動かす（ダブルクリックで元に戻す）
        let (bw, dw) = e.log_cols_for(r.w);
        let sep = e.sep_at(r.y + 3, r.x + bw).expect("ブランチの列の右の線");
        assert!(sep.horizontal_drag());
        e.drag_sep(&sep, r.y + 3, r.x + 40);
        assert_eq!(e.log_cols_for(r.w).0, 40);
        let sep2 = e.sep_at(r.y + 3, r.x + r.w - dw - 1).expect("中身の列の左の線");
        e.drag_sep(&sep2, r.y + 3, r.x + r.w - 61);
        assert_eq!(e.log_cols_for(r.w).1, 60);
        assert!(e.sep_at(r.y, r.x + 40).is_none(), "見出しの行ではつかまない");
        e.reset_sep(&sep);
        e.reset_sep(&sep2);
        assert_eq!(e.log_cols_for(r.w), (bw, dw));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 本物のリポジトリで: ブランチの列を押して選ぶ・ダブルクリックでそのブランチの履歴・右クリックでチェックアウト・本文を押すと戻る・右クリックのメニューで消す
    #[test]
    fn branches_and_commit_menu() {
        let dir = std::env::temp_dir().join(format!("nagi-logops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| String::from_utf8_lossy(&nagi_workers::git::git(&dir, args).unwrap()).trim().to_string();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "test@example.com"]);
        g(&["config", "user.name", "test"]);
        g(&["config", "core.autocrlf", "false"]);
        for f in ["a", "b", "c"] {
            std::fs::write(dir.join(f), format!("{f}\n")).unwrap();
            g(&["add", "."]);
            g(&["commit", "-q", "-m", &format!("add {f}")]);
            if f == "a" {
                g(&["branch", "topic"]);
            }
        }
        let mut buf = nagi_core::Buffer::open(&dir.join("a")).unwrap();
        buf.path = Some(dir.join("a"));
        let mut e = Editor::new(buf, None);
        e.set_screen(120, 40);
        e.start_git();
        let wait = |e: &mut Editor| {
            for _ in 0..500 {
                e.poll_background();
                if e.panels.log.as_ref().is_some_and(|l| !l.loading && !l.branches.is_empty() && l.rx_detail.is_none()) && e.svc.git.status.is_some() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!("履歴が届かない");
        };
        e.select_bottom_tab(crate::result_grid::BottomKind::Git);
        wait(&mut e);
        let l = e.panels.log.as_ref().unwrap();
        // ブランチの列: 0 行目は「ローカル」の見出し、1 行目 main、2 行目 topic
        assert_eq!((l.branch_at(0), l.branch_at(1), l.branch_at(2)), (None, Some(0), Some(1)));
        // 欄の 0 行目はタブ、1 行目は見出し、2 行目から中身。topic を押すと、その指すコミットへ
        e.bottom_click(4, 2, 1);
        let l = e.panels.log.as_ref().unwrap();
        assert_eq!(l.branch_sel, Some(1));
        assert_eq!(l.current().map(|c| c.subject.as_str()), Some("add a"));
        // ダブルクリックで、そのブランチの履歴だけ（チェックアウトはしない）
        e.bottom_click(4, 2, 2);
        wait(&mut e);
        assert_eq!(g(&["rev-parse", "--abbrev-ref", "HEAD"]), "main", "チェックアウトしない");
        let subjects = |e: &Editor| e.panels.log.as_ref().unwrap().rows.iter().filter_map(|r| r.commit.as_ref().map(|c| c.subject.clone())).collect::<Vec<_>>();
        assert_eq!(subjects(&e), vec!["add a"], "topic の履歴だけ");
        assert_eq!(e.panels.log.as_ref().unwrap().branch_sel, Some(1));
        // もう一度ダブルクリックで全部に戻る（下の欄に収めたまま）
        e.bottom_click(4, 2, 2);
        wait(&mut e);
        assert_eq!(subjects(&e).len(), 3);
        assert!(e.panels.log.as_ref().unwrap().docked);
        // チェックアウトは右クリックのメニューから
        let w = e.bottom_content_rect().unwrap().w;
        assert!(e.log_context(3, 2, w, (5, 2)));
        let i = e.menu.as_ref().unwrap().items.iter().position(|it| it.label == "チェックアウト").unwrap();
        e.menu_run(i);
        e.wait_jobs();
        assert_eq!(g(&["rev-parse", "--abbrev-ref", "HEAD"]), "topic", "{}", e.svc.notify.text);
        wait(&mut e);
        // 本文を押すとエディタへ（どのペインからでも）
        assert_eq!(e.state(), State::Log);
        let r = e.active_rect();
        e.click(0, r.x + e.gutter());
        assert_eq!(e.state(), State::Normal);
        // main に戻って、真ん中の 2 つ目のコミット（add b）を右クリック →「コミットを消す」→ y
        g(&["checkout", "-q", "main"]);
        e.refresh_git();
        e.reload_log();
        wait(&mut e);
        let k = e.panels.log.as_ref().unwrap().visible.iter().position(|&i| e.panels.log.as_ref().unwrap().rows[i].commit.as_ref().is_some_and(|c| c.subject == "add b")).unwrap();
        let w = e.bottom_content_rect().unwrap().w;
        assert!(e.log_context(1 + k, 60, w, (10, 60)));
        assert_eq!(e.state(), State::Menu);
        let i = e.menu.as_ref().unwrap().items.iter().position(|it| it.label == "コミットの破棄").unwrap();
        e.menu_run(i);
        // 確認の窓（はい・いいえ）。y で答える
        assert_eq!(e.state(), State::Menu);
        assert!(e.menu.as_ref().unwrap().title.as_deref().is_some_and(|t| t.contains("消しますか")));
        e.key(Key::Char('y'));
        e.wait_jobs();
        assert_eq!(e.state(), State::Log, "答えたら履歴へ戻る");
        assert_eq!(g(&["log", "--format=%s"]), "add c\nadd a", "{}", e.svc.notify.text);
        // メッセージを編集（:Greword）
        let head = g(&["rev-parse", "--short", "HEAD"]);
        e.git_reword(&format!("{head} add c!"));
        e.wait_jobs();
        assert_eq!(g(&["log", "-1", "--format=%s"]), "add c!");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 本物のリポジトリで: Ctrl+クリックで足す・Shift+クリックで範囲・Esc で外す・右クリックで選んだもの全部へ
    /// （2 つを比べる・まとめてチェリーピック）
    #[test]
    fn multi_select_commits() {
        let dir = std::env::temp_dir().join(format!("nagi-logmulti-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| String::from_utf8_lossy(&nagi_workers::git::git(&dir, args).unwrap()).trim().to_string();
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "test@example.com"]);
        g(&["config", "user.name", "test"]);
        g(&["config", "core.autocrlf", "false"]);
        for f in ["a", "b", "c"] {
            std::fs::write(dir.join(f), format!("{f}\n")).unwrap();
            g(&["add", "."]);
            g(&["commit", "-q", "-m", &format!("add {f}")]);
        }
        g(&["checkout", "-q", "-b", "side", "HEAD~2"]);
        std::fs::write(dir.join("d"), "d\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-q", "-m", "add d"]);
        let mut buf = nagi_core::Buffer::open(&dir.join("a")).unwrap();
        buf.path = Some(dir.join("a"));
        let mut e = Editor::new(buf, None);
        e.set_screen(120, 40);
        e.start_git();
        let wait = |e: &mut Editor, what: &dyn Fn(&Editor) -> bool| {
            for _ in 0..500 {
                e.poll_background();
                if what(e) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!("届かない");
        };
        let ready = |e: &Editor| e.panels.log.as_ref().is_some_and(|l| !l.loading && !l.branches.is_empty() && l.rx_detail.is_none()) && e.svc.git.status.is_some();
        e.select_bottom_tab(crate::result_grid::BottomKind::Git);
        wait(&mut e, &ready);
        // 題名の行（見せている行の番号）。欄の 0 行目はタブ、1 行目は見出しなので、押す行は 2 + k
        let at = |e: &Editor, s: &str| e.panels.log.as_ref().unwrap().visible.iter().position(|&i| e.panels.log.as_ref().unwrap().rows[i].commit.as_ref().is_some_and(|c| c.subject == s)).unwrap();
        let click = |e: &mut Editor, k: usize, mods: nagi_vim::Mods| {
            e.click_mods = mods;
            e.bottom_click(2 + k, 60, 1);
            e.click_mods = Default::default();
        };
        let ctrl = nagi_vim::Mods { ctrl: true, ..Default::default() };
        let shift = nagi_vim::Mods { shift: true, ..Default::default() };
        let subjects = |e: &Editor| e.panels.log.as_ref().unwrap().picked().into_iter().map(|c| c.subject).collect::<Vec<_>>();
        let (ka, kb, kc) = (at(&e, "add a"), at(&e, "add b"), at(&e, "add c"));
        // 押すと 1 つ、Ctrl+クリックで今のものに足す（並びは履歴の順）
        click(&mut e, kc, Default::default());
        assert_eq!(subjects(&e), vec!["add c"]);
        click(&mut e, kb, ctrl);
        assert_eq!(subjects(&e), vec!["add c", "add b"]);
        // Ctrl+クリックをもう一度で外す
        click(&mut e, kb, ctrl);
        assert_eq!(subjects(&e), vec!["add c"]);
        // Shift+クリックで、起点から押したところまで
        click(&mut e, kc, Default::default());
        click(&mut e, ka, shift);
        let l = e.panels.log.as_ref().unwrap();
        let want: Vec<String> = (kc.min(ka)..=kc.max(ka)).filter_map(|k| l.commit_at(k).map(|c| c.subject.clone())).collect();
        assert_eq!(subjects(&e), want);
        assert!(want.len() >= 3);
        // Esc で選びを外す（履歴は閉じない）
        e.key(Key::Esc);
        assert!(e.panels.log.as_ref().unwrap().marked.is_empty());
        assert_eq!(e.state(), State::Log);
        // 2 つを選んで右クリック → 比べる: 右の列に、a から c までに変わったファイル
        click(&mut e, ka, Default::default());
        click(&mut e, kc, ctrl);
        let w = e.bottom_content_rect().unwrap().w;
        assert!(e.log_context(1 + ka, 60, w, (10, 60)));
        let labels: Vec<String> = e.menu.as_ref().unwrap().items.iter().map(|it| it.label.clone()).collect();
        assert!(labels.contains(&"2 件を cherry-pick".to_string()), "{labels:?}");
        assert_eq!(e.panels.log.as_ref().unwrap().marked.len(), 2, "右クリックで選びを外さない");
        let i = labels.iter().position(|s| s == "バージョンの比較").unwrap();
        e.menu_run(i);
        wait(&mut e, &|e| e.panels.log.as_ref().unwrap().detail.as_ref().is_some_and(|d| d.base.is_some()));
        let l = e.panels.log.as_ref().unwrap();
        let d = l.detail.as_ref().unwrap();
        assert_eq!(l.current().unwrap().subject, "add c", "新しい方を今の行に");
        assert_eq!(d.files, vec![('A', "b".to_string()), ('A', "c".to_string())]);
        // b と c を選んで、まとめてチェリーピック（古い順に）
        let (kb, kc) = (at(&e, "add b"), at(&e, "add c"));
        click(&mut e, kb, Default::default());
        click(&mut e, kc, ctrl);
        assert!(e.log_context(1 + kb, 60, w, (10, 60)));
        let i = e.menu.as_ref().unwrap().items.iter().position(|it| it.label == "2 件を cherry-pick").unwrap();
        e.menu_run(i);
        assert!(e.menu.as_ref().unwrap().title.as_deref().is_some_and(|t| t.contains("2 件のコミット")));
        e.key(Key::Char('y'));
        e.wait_jobs();
        assert_eq!(g(&["log", "--format=%s", "side"]), "add c\nadd b\nadd d\nadd a", "{}", e.svc.notify.text);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
