//! 言語の差し込み口（`Language`）と、手順の間で受け渡すもの。
//!
//! 共通の手順（template）が順に呼び、言語は違うところだけを返す。分からなければ空・None を返し、手順は次の段へ進む。

use std::path::{Path, PathBuf};

use nagi_syntax::defs::DefKind;
use nagi_workers::index::{Entry, Index};

use crate::project::Ranker;

use crate::libs::Libraries;
use crate::text::Lexis;

/// 補完の候補 1 つ
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    /// 一覧の印（m メソッド・f フィールド・c クラス・I インターフェース・e 列挙・K 予約語・v 語・p パッケージ）
    pub mark: char,
    /// 右に出す説明（持っている型など）
    pub note: String,
    /// メソッドなら Some(引数があるか)。決めたときに () を入れる
    pub call: Option<bool>,
    /// 同じ一致の度合いの中での順（小さいほど先）
    pub rank: u8,
    /// 決めたときに入れる文字（名前と違うとき。SQL の "名前" など）
    pub insert: Option<String>,
    /// 決めたときに足す import の鍵（中身は言語だけが知る。Java は完全な名前、TypeScript はモジュール）
    pub import: Option<String>,
}

impl Item {
    /// 印・説明・順だけの候補
    pub fn new(name: impl Into<String>, mark: char, note: impl Into<String>, rank: u8) -> Item {
        Item { name: name.into(), mark, note: note.into(), call: None, rank, insert: None, import: None }
    }

    /// メソッド・フィールド（`call` は Some(引数があるか)）
    pub fn member(name: impl Into<String>, method: bool, params: bool, owner: impl Into<String>, rank: u8) -> Item {
        Item { name: name.into(), mark: if method { 'm' } else { 'f' }, note: owner.into(), call: method.then_some(params), rank, insert: None, import: None }
    }
}

/// 定義の種類の印
pub fn mark_of(kind: DefKind) -> char {
    match kind {
        DefKind::Method | DefKind::Function => 'm',
        DefKind::Field | DefKind::Constant | DefKind::Variable => 'f',
        DefKind::Interface => 'I',
        DefKind::Enum => 'e',
        DefKind::Class | DefKind::Type => 'c',
        DefKind::Module => 'v',
    }
}

/// import の足し方
#[derive(Debug, PartialEq, Eq)]
pub enum Import {
    /// 足さなくてよい
    None,
    /// (byte の位置, 入れる文字)
    Insert(usize, String),
    /// 足せない（同じ名前の別のものを import している）。名前の代わりにこれを書く
    Qualified(String),
}

/// 補完する場所
pub struct Cx<'a> {
    /// 言語
    pub lang: &'a dyn Language,
    /// ファイルの中身
    pub src: &'a str,
    /// ファイルの絶対パス
    pub here: Option<&'a Path>,
    /// 今の行
    pub line: &'a str,
    /// 打っている語の始まり（行の中の byte）
    pub ws: usize,
    /// 打っている語の始まり（絶対 byte）
    pub start: usize,
    /// 打った語
    pub prefix: &'a str,
    /// プロジェクトの索引
    pub index: Option<&'a Index>,
    /// 索引の根からの相対パス（/ 区切り。索引の外なら空）
    pub rel: String,
    /// 今いる型
    pub inside: Option<String>,
    /// ライブラリの索引
    pub libs: &'a Libraries,
    /// 設定: ファイルの中の語を出す・予約語を出す
    pub words: bool,
    pub keywords: bool,
    /// 言語が作るファイル（ライブラリのソース・骨組み）を置く場所（None なら言語の既定）
    pub out_dir: Option<&'a Path>,
    /// SQL のコンソール（言語がパスでなく接続で決まり、ライブラリが接続に付く）
    pub console: Option<crate::sql::Console<'a>>,
}

impl<'a> Cx<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(lang: &'a dyn Language, src: &'a str, here: Option<&'a Path>, line: &'a str, ws: usize, start: usize, prefix: &'a str, index: Option<&'a Index>, libs: &'a Libraries) -> Self {
        let rel = match (index, here) {
            (Some(i), Some(h)) => h.strip_prefix(&i.root).ok().map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default(),
            _ => String::new(),
        };
        let inside = crate::project::enclosing_type(lang.lexis(), src, start);
        Cx { lang, src, here, line, ws, start, prefix, index, rel, inside, libs, words: true, keywords: true, out_dir: None, console: None }
    }

    /// `at`（名前の始まりの絶対 byte）の場所（定義へ・参照の絞り込み）
    pub fn at(lang: &'a dyn Language, src: &'a str, here: Option<&'a Path>, at: usize, index: Option<&'a Index>, libs: &'a Libraries) -> Self {
        let ls = src[..at].rfind('\n').map_or(0, |i| i + 1);
        let line = src[ls..].split('\n').next().unwrap_or("").trim_end_matches('\r');
        Cx::new(lang, src, here, line, at - ls, at, "", index, libs)
    }

    /// `.` の後か
    pub fn after_dot(&self) -> bool {
        self.ws > 0 && self.line[..self.ws].ends_with('.')
    }
}

/// 定義の場所
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
    /// 跳んだときに伝えること（ライブラリのソースか骨組みかなど）
    pub note: Option<String>,
}

/// `.` の前の式を広げたもの
pub enum Head {
    /// 広げた式と、頭のパッケージの名前を外したときの (単純名, 完全な名前)
    Expr(String, Option<(String, String)>),
    /// パッケージの名前だけだった: その中身
    Items(Vec<Item>),
}

/// ライブラリのメンバー
pub enum LibMember {
    Item(Item),
    /// プロジェクトの型（そのメンバーは手順がプロジェクトの索引から出す）
    Project(String),
}

/// `.` の後でないときの、言語の名前（ライブラリの型・ほかのファイルの名前）
#[derive(Default)]
pub struct ScopeNames {
    /// プロジェクトの型の前に出す（同じ名前の後の候補を隠す。import を伴う TypeScript の名前）
    pub first: Vec<Item>,
    /// プロジェクトの型の後に出す（同じ名前でも別のものとして並べる。Java のライブラリの型）
    pub last: Vec<Item>,
}

/// 言語
pub trait Language: Sync {
    /// 名前（記録・試験用）
    fn name(&self) -> &'static str;

    /// 字句の規則
    fn lexis(&self) -> &'static Lexis;

    /// よく使う予約語（補完に出す）
    fn keywords(&self) -> &'static [&'static str] {
        &[]
    }

    /// 補完を始める文字（打ったら一覧を出す）
    fn triggers(&self) -> &'static [char] {
        &['.']
    }

    /// 言語の手順が丸ごと違うときの候補（SQL）。None なら共通の手順で
    fn complete_all(&self, _cx: &Cx) -> Option<Vec<Item>> {
        None
    }

    /// `.` の前の式（行の中の `before`）を広げる（前の行からの連なり・型を書かない変数・パッケージの名前）
    fn head(&self, cx: &Cx, before: &str) -> Head {
        Head::Expr(crate::text::with_previous_lines(self.lexis(), cx.src, cx.start - cx.ws, before), None)
    }

    /// 変数 `name` の宣言の型の文字（型引数つき）。受け手がクラスの名前か変数かを分ける
    fn decl_text(&self, _src: &str, _name: &str, _at: usize) -> Option<String> {
        None
    }

    /// プロジェクトの型 `ty` が、ライブラリの親から継ぐメンバー（`rank` の順で）
    fn inherited(&self, _cx: &Cx, _ranker: &mut Ranker, _ty: &str, _rank: u8) -> Vec<Item> {
        Vec::new()
    }

    /// `.` の前の式 `expanded` のライブラリのメンバー。`statics` は受け手がクラスの名前か、
    /// `found` はプロジェクトの索引で候補が見つかったか
    fn library_members(&self, _cx: &Cx, _ranker: &mut Ranker, _expanded: &str, _fqn: Option<&(String, String)>, _statics: bool, _found: bool) -> Vec<LibMember> {
        Vec::new()
    }

    /// クラスの名前の後に出す予約語（Java の `class`）
    fn class_keywords(&self) -> &'static [&'static str] {
        &[]
    }

    /// `.` の後でないときの、言語の名前
    fn scope_names(&self, _cx: &Cx) -> ScopeNames {
        ScopeNames::default()
    }

    /// プロジェクトの外（ライブラリ・.d.ts）の、`cx.start` の名前 `name` の定義
    fn library_definition(&self, _cx: &Cx, _name: &str) -> Option<Target> {
        None
    }

    /// 参照の絞り込み: `cx.start` の名前 `name` が、ライブラリのもの（受け手の型がライブラリ）か
    fn library_site(&self, _cx: &Cx, _name: &str) -> bool {
        false
    }

    // ---- プロジェクトの索引で型を求める（Ranker）。定義のあるファイルの言語で呼ぶ

    /// 使う生態系（同じ生態系の言語どうしは、定義の候補で同じまとまり。Java と Kotlin、TypeScript と JavaScript）
    fn ecosystem(&self) -> Option<&'static str> {
        None
    }

    /// 定義の行 `line` の `col` にある名前の宣言の型（フィールドの型・メソッドの戻り値の型。総称型は外側）。既定は C 系（型を名前の前に書く）
    fn decl_type(&self, line: &str, col: usize) -> Option<String> {
        crate::project::c_decl_type(&line[..col])
    }

    /// 引数 1 つの宣言の型。既定は C 系
    fn param_type(&self, p: &str) -> Option<String> {
        crate::project::c_param_type(p)
    }

    /// 同じファイルの同じ名前の定義が、引数で選ぶオーバーロードか（Java・Kotlin）
    fn overloads(&self) -> bool {
        false
    }

    /// 別のファイルから使える定義か（定義の行。TypeScript は export したものだけ）
    fn exported(&self, _decl_line: &str) -> bool {
        true
    }

    /// 書いていないメンバー（名前, 引数の数）。型 `ty` の定義の行 `line`、型の中の定義 `defs`（Java の Lombok・record）
    fn implicit_members(&self, _lines: &[String], _line: usize, _ty: &str, _defs: &[Entry]) -> Vec<(String, usize)> {
        Vec::new()
    }

    /// 書いていないメソッド `name` の元のフィールド（Lombok の getFoo → foo）。呼んだ側のファイルの言語で
    fn accessor_field(&self, _name: &str) -> Option<String> {
        None
    }

    /// 組み込みの型の親（JDK のよくある型。オーバーロードを引数の型で選ぶ）。呼んだ側のファイルの言語で
    fn builtin_supers(&self, _t: &str) -> Option<&'static [&'static str]> {
        None
    }

    /// 親が分かっている組み込みの型か
    fn builtin_known(&self, _t: &str) -> bool {
        false
    }

    /// 候補の import の鍵 `key` を、ファイル `src` で名前 `name` に使うための足し方
    fn import_plan(&self, _src: &str, _name: &str, _key: &str) -> Import {
        Import::None
    }
}
