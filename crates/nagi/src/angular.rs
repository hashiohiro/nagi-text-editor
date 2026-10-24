//! Angular のテンプレート（.html）から定義へ跳ぶ（gd・F12・Ctrl+クリック）。
//!
//! - 式の中の名前（{{ title }}、(click)="onGo()"、*ngIf="isShown"、@if (user) …）→ このテンプレートの部品（.component.ts）
//!   のメソッド・プロパティ。部品に無ければ親クラスへたどる（基底の部品クラスのメソッドなど）
//! - 部品のタグ（<app-button>）→ selector が同じ部品のクラス
//! - 部品の属性（[width]、(clicked)）→ その部品の @Input・@Output のプロパティ
//!
//! `x.y` の `y` のように受け手のあるもの、テンプレートの中の変数（let item of items、#ref）は扱わず、いつもの gd に任せる。

use std::path::{Path, PathBuf};

use nagi_syntax::HtmlCtx;
use nagi_syntax::defs::Receiver;

use crate::editor::Editor;

/// テンプレートの部品の .ts（同じ名前の .ts、無ければ同じフォルダで templateUrl がこのファイルを指すもの）
pub fn component_of(html: &Path) -> Option<PathBuf> {
    let ts = html.with_extension("ts");
    if ts.is_file() {
        return Some(ts);
    }
    let name = html.file_name()?.to_string_lossy().into_owned();
    std::fs::read_dir(html.parent()?).ok()?.flatten().map(|e| e.path()).find(|p| {
        p.extension().is_some_and(|e| e == "ts") && std::fs::read_to_string(p).is_ok_and(|s| s.contains("templateUrl") && s.contains(&name))
    })
}

/// selector が `tag` の部品の .ts（プロジェクトの .component.ts から探す）
pub fn component_by_selector(root: &Path, tag: &str) -> Option<PathBuf> {
    let pats = [format!("selector: '{tag}'"), format!("selector: \"{tag}\""), format!("selector:'{tag}'"), format!("selector: `{tag}`")];
    for e in nagi_workers::files::walker(root).build().flatten() {
        let p = e.path();
        if !p.to_string_lossy().ends_with(".ts") || p.to_string_lossy().ends_with(".spec.ts") {
            continue;
        }
        if let Ok(s) = std::fs::read_to_string(p) {
            if s.contains("@Component") && pats.iter().any(|x| s.contains(x.as_str())) {
                return Some(p.to_path_buf());
            }
        }
    }
    None
}

impl Editor {
    /// Angular のテンプレートで跳べたら true（跳べなければいつもの gd へ）
    pub(crate) fn angular_goto(&mut self, name: &str, word_at: usize) -> bool {
        let Some(html) = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok()) else { return false };
        let Some(ctx) = self.syntax.as_ref().and_then(|s| s.html_context(&self.buf, word_at)) else { return false };
        let Some(idx) = self.nav.index.as_ref() else { return false };
        let root = idx.root.clone();
        let rel = |p: &Path| p.strip_prefix(&root).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
        // (部品の .ts, 探す名前, 受け手)。タグはクラスそのもの
        let (ts, member) = match &ctx {
            HtmlCtx::Tag(t) if t.contains('-') => match component_by_selector(&root, t) {
                Some(ts) => (ts, None),
                None => {
                    self.notify.warn(format!("部品が見つかりません: <{t}>（selector: '{t}' の @Component）"));
                    return true;
                }
            },
            HtmlCtx::Attr { tag, name } if tag.contains('-') && !name.is_empty() => match component_by_selector(&root, tag) {
                Some(ts) => (ts, Some(name.clone())),
                None => return false,
            },
            HtmlCtx::Other => {
                // 受け手のある名前（user.name の name）はいつもの gd に任せる
                let before = self.buf.slice_string(word_at.saturating_sub(2), word_at);
                if before.ends_with('.') || before.ends_with("?.") {
                    return false;
                }
                match component_of(&html) {
                    Some(ts) => (ts, Some(name.to_string())),
                    None => return false,
                }
            }
            _ => return false,
        };
        let ts_rel = rel(&ts);
        let src = std::fs::read_to_string(&ts).unwrap_or_default();
        let found = match &member {
            // 部品のクラス（ファイルの中の最初のクラス）
            None => {
                let defs: Vec<nagi_workers::index::Loc> = idx.all().filter(|(_, l)| &*l.path == ts_rel && l.kind.is_type()).map(|(_, l)| l.clone()).collect();
                defs.into_iter().min_by_key(|l| l.line)
            }
            // 部品のメンバー（無ければ親クラスへ）
            Some(m) => match nagi_lang::project::Ranker::new(Some(idx)).resolve(&ts_rel, &src, m, Some(&Receiver::This), None, idx.lookup(m).to_vec()) {
                nagi_lang::project::Ranked::Locs(v, best) if best >= 1 => v.into_iter().next().filter(|l| *l.path == *ts_rel || !idx.lookup(m).is_empty()),
                _ => None,
            },
        };
        match found {
            Some(l) if member.is_none() || *l.path == *ts_rel || self.in_hierarchy(&ts_rel, &src, &l) => {
                let p = root.join(&*l.path);
                self.jump_to(&p, l.line as usize, l.col as usize);
                true
            }
            _ => {
                if let Some(m) = &member {
                    // テンプレートの中の変数（let x、#ref）かもしれないので、いつもの gd に任せる
                    if matches!(ctx, HtmlCtx::Other) {
                        return false;
                    }
                    self.notify.info(format!("{} に {m} がありません", ts.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned())));
                }
                true
            }
        }
    }

    /// `l` が部品のクラスか、その親クラスのファイルにあるか（同じ名前の別のファイルへ跳ばない）
    fn in_hierarchy(&self, ts_rel: &str, src: &str, l: &nagi_workers::index::Loc) -> bool {
        let Some(idx) = self.nav.index.as_ref() else { return false };
        let mut types: Vec<String> = nagi_lang::project::supertypes(src, None);
        let mut seen = std::collections::HashSet::new();
        while let Some(t) = types.pop() {
            if !seen.insert(t.clone()) || seen.len() > 16 {
                continue;
            }
            for d in idx.lookup(&t).iter().filter(|d| d.kind.is_type()) {
                if d.path == l.path {
                    return true;
                }
                if let Ok(s) = std::fs::read_to_string(idx.root.join(&*d.path)) {
                    types.extend(nagi_lang::project::supertypes(&s, Some(&t)));
                }
            }
        }
        *l.path == *ts_rel
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    fn keys(e: &mut Editor, k: &str) {
        for key in nagi_vim::parse_keys(k, " ") {
            e.key(key);
        }
    }

    #[test]
    fn template_to_component() {
        let dir = std::env::temp_dir().join(format!("nagi-ng-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("src/page")).unwrap();
        std::fs::create_dir_all(dir.join("src/button")).unwrap();
        std::fs::create_dir_all(dir.join("src/base")).unwrap();
        unsafe { std::env::set_var("NAGI_INDEX_DIR", dir.join("cache")) };
        std::fs::write(dir.join("src/base/page-base.ts"), "export abstract class PageBase {\n  goBack(): void {}\n}\n").unwrap();
        std::fs::write(
            dir.join("src/page/page.component.ts"),
            "@Component({ selector: 'app-page', templateUrl: './page.component.html' })\nexport class PageComponent extends PageBase {\n  title = 'x';\n  onGo(n: number) {}\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/button/button.component.ts"), "@Component({ selector: 'app-button', template: '' })\nexport class ButtonComponent {\n  @Input() width = 0;\n}\n").unwrap();
        // 別の部品の同じ名前（こちらへは跳ばない）
        std::fs::write(dir.join("src/other.ts"), "export class Other {\n  onGo() {}\n  title = 'y';\n}\n").unwrap();
        std::fs::write(dir.join("src/page/page.component.html"), "<p>{{ title }}</p>\n<app-button [width]=\"300\" (click)=\"onGo(1)\"></app-button>\n<a (click)=\"goBack()\">戻る</a>\n").unwrap();
        let path = dir.join("src/page/page.component.html");
        let mut buf = Buffer::open(&path).unwrap();
        buf.path = Some(path);
        let mut e = Editor::new(buf, None);
        e.set_screen(120, 40);
        e.start_index();
        for _ in 0..1000 {
            e.poll_index();
            if e.nav.index.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        e.ensure_syntax();
        let at = |e: &Editor| (e.buf.path.as_ref().unwrap().file_name().unwrap().to_string_lossy().into_owned(), e.cursor.line);
        // {{ title }} → 部品のプロパティ
        keys(&mut e, "gg/title<CR>gd");
        assert_eq!(at(&e), ("page.component.ts".into(), 2), "{}", e.notify.text);
        // (click)="onGo(1)" → 部品のメソッド
        keys(&mut e, "<C-o>2G0/onGo<CR>gd");
        assert_eq!(at(&e), ("page.component.ts".into(), 3), "{}", e.notify.text);
        // <app-button> → 部品のクラス
        keys(&mut e, "<C-o>2G0wgd");
        assert_eq!(at(&e), ("button.component.ts".into(), 1), "{}", e.notify.text);
        // [width] → 部品の @Input
        keys(&mut e, "<C-o>2G0/width<CR>gd");
        assert_eq!(at(&e), ("button.component.ts".into(), 2), "{}", e.notify.text);
        // goBack() → 親クラスのメソッド
        keys(&mut e, "<C-o>3G0/goBack<CR>gd");
        assert_eq!(at(&e), ("page-base.ts".into(), 1), "{}", e.notify.text);
    }
}
