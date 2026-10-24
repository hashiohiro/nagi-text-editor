//! Maven の依存の jar を、mvn を起動せずに求める（pom.xml と手元のリポジトリ `~\.m2\repository` を読む）。
//!
//! Maven 3 と同じ考え方で: 親の継承（relativePath、無ければリポジトリ）、`${...}` の置き換え（継承してから子の文脈で。
//! `.mvn/maven.config` の -D が優先）、dependencyManagement（import の BOM も。明示したものが勝つ）、推移的な依存
//! （近い方が勝ち、同じ深さなら先に書いた方。provided・test・optional は辿らない。exclusions は経路で積む。
//! 根の dependencyManagement が推移的な依存の版を決める）。プロファイルは activeByDefault・`!prop`・jdk・os・file の条件だけ見る。
//! ダウンロードはしない（手元に無い pom・jar は missing に数え、呼んだ側が mvn に任せるか決める）。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

// ---- XML（pom に要る分だけ: 要素・文字・コメント・CDATA。属性は読まない）----

#[derive(Default, Debug)]
struct Xml {
    name: String,
    text: String,
    kids: Vec<Xml>,
}

impl Xml {
    fn get(&self, name: &str) -> Option<&Xml> {
        self.kids.iter().find(|k| k.name == name)
    }
    fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Xml> {
        self.kids.iter().filter(move |k| k.name == name)
    }
    /// 子の要素の文字（無ければ空）
    fn s(&self, name: &str) -> String {
        self.get(name).map(|k| k.text.trim().to_string()).unwrap_or_default()
    }
    /// `<a><b/><b/></a>` の b たち
    fn list<'a>(&'a self, outer: &str, inner: &'a str) -> impl Iterator<Item = &'a Xml> {
        self.get(outer).into_iter().flat_map(move |o| o.all(inner))
    }
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn parse_xml(text: &str) -> Option<Xml> {
    let mut stack = vec![Xml::default()];
    let mut rest = text;
    while let Some(lt) = rest.find('<') {
        let content = &rest[..lt];
        if !content.trim().is_empty() {
            stack.last_mut()?.text.push_str(&unescape(content));
        }
        rest = &rest[lt..];
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = r.find("-->").map_or("", |i| &r[i + 3..]);
            continue;
        }
        if let Some(r) = rest.strip_prefix("<![CDATA[") {
            let i = r.find("]]>")?;
            stack.last_mut()?.text.push_str(&r[..i]);
            rest = &r[i + 3..];
            continue;
        }
        if rest.starts_with("<?") || rest.starts_with("<!") {
            rest = rest.find('>').map_or("", |i| &rest[i + 1..]);
            continue;
        }
        let gt = rest.find('>')?;
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];
        if tag.starts_with('/') {
            if stack.len() < 2 {
                return None;
            }
            let done = stack.pop()?;
            stack.last_mut()?.kids.push(done);
            continue;
        }
        let empty = tag.ends_with('/');
        let name = tag.trim_end_matches('/').split(char::is_whitespace).next().unwrap_or("");
        let el = Xml { name: name.to_string(), ..Default::default() };
        if empty {
            stack.last_mut()?.kids.push(el);
        } else {
            stack.push(el);
        }
    }
    if stack.len() != 1 {
        return None;
    }
    stack.pop()?.kids.pop()
}

// ---- 版 ----

/// Maven の版の比べ方（数は数で、文字は alpha < beta < milestone < rc < snapshot < 無し < sp の順。ほかの文字はその後ろで辞書順）
fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fn items(v: &str) -> Vec<Result<u64, String>> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut digit = None;
        let lower = v.to_ascii_lowercase();
        for c in lower.chars() {
            if c == '.' || c == '-' || c == '_' {
                if !cur.is_empty() {
                    out.push(cur.parse::<u64>().map_err(|_| cur.clone()));
                }
                cur.clear();
                digit = None;
                continue;
            }
            let d = c.is_ascii_digit();
            if digit.is_some_and(|x| x != d) {
                out.push(cur.parse::<u64>().map_err(|_| cur.clone()));
                cur.clear();
            }
            digit = Some(d);
            cur.push(c);
        }
        if !cur.is_empty() {
            out.push(cur.parse::<u64>().map_err(|_| cur.clone()));
        }
        // 末尾の 0 と、リリースを表す語は無いのと同じ
        while matches!(out.last(), Some(Ok(0))) || matches!(out.last(), Some(Err(s)) if matches!(s.as_str(), "final" | "ga" | "release")) {
            out.pop();
        }
        out
    }
    fn rank(s: &str) -> (u8, &str) {
        match s {
            "alpha" | "a" => (0, ""),
            "beta" | "b" => (1, ""),
            "milestone" | "m" => (2, ""),
            "rc" | "cr" => (3, ""),
            "snapshot" => (4, ""),
            "" => (5, ""),
            "sp" => (6, ""),
            _ => (7, s),
        }
    }
    let (x, y) = (items(a), items(b));
    for i in 0..x.len().max(y.len()) {
        let o = match (x.get(i), y.get(i)) {
            (Some(Ok(p)), Some(Ok(q))) => p.cmp(q),
            (Some(Ok(_)), Some(Err(_))) => std::cmp::Ordering::Greater,
            (Some(Err(_)), Some(Ok(_))) => std::cmp::Ordering::Less,
            (Some(Err(p)), Some(Err(q))) => rank(p).cmp(&rank(q)),
            (Some(Ok(p)), None) => p.cmp(&0),
            (None, Some(Ok(q))) => 0.cmp(q),
            (Some(Err(p)), None) => rank(p).cmp(&rank("")),
            (None, Some(Err(q))) => rank("").cmp(&rank(q)),
            (None, None) => std::cmp::Ordering::Equal,
        };
        if o.is_ne() {
            return o;
        }
    }
    std::cmp::Ordering::Equal
}

/// 版の範囲（`[1.0,2.0)`・`[1.5]`・`(,1.0],[1.2,)`）に `v` が入るか
fn in_range(range: &str, v: &str) -> bool {
    let mut rest = range.trim();
    while !rest.is_empty() {
        let Some(close) = rest.find([']', ')']) else {
            return false;
        };
        let part = &rest[..=close];
        rest = rest[close + 1..].trim_start_matches(',').trim();
        let (lo_inc, hi_inc) = (part.starts_with('['), part.ends_with(']'));
        let inner = &part[1..part.len() - 1];
        let ok = match inner.split_once(',') {
            None => version_cmp(v, inner.trim()).is_eq(),
            Some((lo, hi)) => {
                let (lo, hi) = (lo.trim(), hi.trim());
                let above = lo.is_empty() || if lo_inc { version_cmp(v, lo).is_ge() } else { version_cmp(v, lo).is_gt() };
                let below = hi.is_empty() || if hi_inc { version_cmp(v, hi).is_le() } else { version_cmp(v, hi).is_lt() };
                above && below
            }
        };
        if ok {
            return true;
        }
    }
    false
}

// ---- モデル ----

#[derive(Clone, Debug, Default)]
struct Dep {
    group: String,
    artifact: String,
    version: String,
    scope: String,
    classifier: String,
    typ: String,
    optional: bool,
    exclusions: Vec<(String, String)>,
    system_path: String,
}

impl Dep {
    fn read(x: &Xml) -> Dep {
        Dep {
            group: x.s("groupId"),
            artifact: x.s("artifactId"),
            version: x.s("version"),
            scope: x.s("scope"),
            classifier: x.s("classifier"),
            typ: x.s("type"),
            optional: x.s("optional") == "true",
            exclusions: x.list("exclusions", "exclusion").map(|e| (e.s("groupId"), e.s("artifactId"))).collect(),
            system_path: x.s("systemPath"),
        }
    }
    fn typ(&self) -> &str {
        if self.typ.is_empty() { "jar" } else { &self.typ }
    }
    /// 管理と衝突の単位（group:artifact:type:classifier）
    fn key(&self) -> String {
        let classifier = if self.classifier.is_empty() && self.typ() == "test-jar" { "tests" } else { &self.classifier };
        let typ = if self.typ() == "test-jar" { "jar" } else { self.typ() };
        format!("{}:{}:{typ}:{classifier}", self.group, self.artifact)
    }
    fn map(&mut self, f: &dyn Fn(&str) -> String) {
        for s in [&mut self.group, &mut self.artifact, &mut self.version, &mut self.scope, &mut self.classifier, &mut self.typ, &mut self.system_path] {
            if s.contains("${") {
                *s = f(s);
            }
        }
        for (g, a) in &mut self.exclusions {
            *g = f(g);
            *a = f(a);
        }
    }
}

/// pom 1 つの中身（置き換える前。合うプロファイルを足した）。親を指して継承を表す（親の中身は写さない）
#[derive(Debug, Default)]
struct Layer {
    group: String,
    artifact: String,
    version: String,
    packaging: String,
    parent: Option<Rc<Layer>>,
    parent_gav: Option<(String, String, String)>,
    props: Vec<(String, String)>,
    deps: Vec<Dep>,
    managed: Vec<Dep>,
}

impl Layer {
    /// 根の親から自分まで
    fn chain(&self) -> Vec<&Layer> {
        let mut v = vec![self];
        let mut p = self.parent.as_deref();
        while let Some(l) = p {
            v.push(l);
            p = l.parent.as_deref();
        }
        v.reverse();
        v
    }

    /// 自分に無ければ親の値
    fn inherited(&self, f: impl Fn(&Layer) -> &String) -> String {
        self.chain().iter().rev().map(|l| f(l)).find(|s| !s.is_empty()).cloned().unwrap_or_default()
    }
}

/// 置き換えて、管理を当てたモデル
#[derive(Debug)]
struct Model {
    layer: Rc<Layer>,
    dir: Option<PathBuf>,
    group: String,
    artifact: String,
    deps: Vec<Dep>,
    modules: Vec<String>,
    /// 管理の全体（import の BOM も）。BOM に使うときだけ作る
    managed: std::cell::OnceCell<Rc<HashMap<String, Dep>>>,
}

/// 継承した管理の引き方（明示したものは置き換える前のまま指し、import は置き換えて順に）
struct ManagedIndex<'a> {
    explicit: HashMap<String, &'a Dep>,
    imports: Vec<Dep>,
    /// imports の BOM の管理（初めて引くときに読む）
    boms: Option<Vec<Rc<HashMap<String, Dep>>>>,
}

impl<'a> ManagedIndex<'a> {
    fn new(chain: &[&'a Layer], f: &dyn Fn(&str) -> String) -> Self {
        let mut explicit = HashMap::new();
        let mut imports: Vec<Dep> = Vec::new();
        for l in chain {
            for d in &l.managed {
                let mut k = Dep { group: d.group.clone(), artifact: d.artifact.clone(), typ: d.typ.clone(), classifier: d.classifier.clone(), scope: d.scope.clone(), ..Default::default() };
                k.map(f);
                if k.scope == "import" && k.typ() == "pom" {
                    let mut d = d.clone();
                    d.map(f);
                    match imports.iter_mut().find(|x| x.key() == d.key()) {
                        Some(x) => *x = d,
                        None => imports.push(d),
                    }
                } else {
                    explicit.insert(k.key(), d);
                }
            }
        }
        ManagedIndex { explicit, imports, boms: None }
    }
}

fn merge_deps(into: &mut Vec<Dep>, more: &[Dep]) {
    if more.is_empty() {
        return;
    }
    let mut at: HashMap<String, usize> = into.iter().enumerate().map(|(i, d)| (d.key(), i)).collect();
    for d in more {
        match at.get(&d.key()) {
            Some(&i) => into[i] = d.clone(),
            None => {
                at.insert(d.key(), into.len());
                into.push(d.clone());
            }
        }
    }
}

/// 依存を求める。1 回の resolve の間だけ pom を覚えておく
pub struct Resolver {
    repo: PathBuf,
    /// `-D` の値（`.mvn/maven.config`）
    user: HashMap<String, String>,
    /// JDK の版（プロファイルの jdk の条件に）
    java: u32,
    models: HashMap<String, Option<Rc<Model>>>,
    busy: HashSet<String>,
    /// jar の有る無し
    files: HashMap<PathBuf, bool>,
    /// 管理を作っているモデル（import の巡りを止める）
    managing: HashSet<usize>,
    /// group:artifact → reactor のモジュールの pom.xml
    reactor: HashMap<String, PathBuf>,
    pub missing: Vec<String>,
}

/// 求めた依存
#[derive(Debug, Default)]
pub struct Resolved {
    pub jars: Vec<PathBuf>,
    /// 手元に無かったもの（group:artifact:version と何が無いか）
    pub missing: Vec<String>,
    /// 読んだモジュールの数
    pub modules: usize,
    /// 読んだ pom の数
    pub poms: usize,
}

/// 手元のリポジトリ（`~\.m2\settings.xml` の localRepository。無ければ `~\.m2\repository`）
pub fn local_repo() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from)?;
    let m2 = home.join(".m2");
    if let Some(x) = std::fs::read_to_string(m2.join("settings.xml")).ok().and_then(|t| parse_xml(&t)) {
        let r = x.s("localRepository");
        if !r.is_empty() {
            return Some(PathBuf::from(r.replace("${user.home}", &home.display().to_string())));
        }
    }
    Some(m2.join("repository"))
}

/// `.mvn/maven.config` の `-Dk=v`・`-D k=v`・`--define k=v`
fn maven_config(root: &Path) -> HashMap<String, String> {
    let text = std::fs::read_to_string(root.join(".mvn").join("maven.config")).unwrap_or_default();
    let mut out = HashMap::new();
    let mut words = text.split_whitespace();
    while let Some(w) = words.next() {
        let kv = match w.strip_prefix("-D") {
            Some("") => words.next(),
            Some(kv) => Some(kv),
            None if w == "--define" => words.next(),
            None => None,
        };
        if let Some(kv) = kv {
            let (k, v) = kv.split_once('=').unwrap_or((kv, "true"));
            out.insert(k.to_string(), v.trim_matches('"').to_string());
        }
    }
    out
}

impl Resolver {
    pub fn new(repo: PathBuf, root: &Path, java: u32) -> Self {
        Resolver { repo, user: maven_config(root), java, models: HashMap::new(), busy: HashSet::new(), managing: HashSet::new(), files: HashMap::new(), reactor: HashMap::new(), missing: Vec::new() }
    }

    fn artifact_dir(&self, g: &str, a: &str) -> PathBuf {
        let mut p = self.repo.clone();
        for part in g.split('.') {
            p.push(part);
        }
        p.push(a);
        p
    }

    fn artifact_file(&self, g: &str, a: &str, v: &str, classifier: &str, ext: &str) -> PathBuf {
        let c = if classifier.is_empty() { String::new() } else { format!("-{classifier}") };
        self.artifact_dir(g, a).join(v).join(format!("{a}-{v}{c}.{ext}"))
    }

    /// 範囲の版を、手元にある版のうち入る一番新しいものに
    fn pick_version(&self, g: &str, a: &str, v: &str) -> String {
        if !v.starts_with(['[', '(']) {
            return v.to_string();
        }
        let dir = self.artifact_dir(g, a);
        std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| in_range(v, n) && dir.join(n).join(format!("{a}-{n}.pom")).is_file())
            .max_by(|x, y| version_cmp(x, y))
            .unwrap_or_else(|| v.to_string())
    }

    fn by_gav(&mut self, g: &str, a: &str, v: &str) -> Option<Rc<Model>> {
        if let Some(p) = self.reactor.get(&format!("{g}:{a}")).cloned() {
            return self.by_file(&p);
        }
        let v = self.pick_version(g, a, v);
        let pom = self.artifact_file(g, a, &v, "", "pom");
        let key = format!("{g}:{a}:{v}");
        if let Some(m) = self.models.get(&key) {
            return m.clone();
        }
        let m = self.load(&pom, None);
        if m.is_none() {
            self.missing.push(format!("{key} (pom)"));
        }
        self.models.insert(key, m.clone());
        m
    }

    fn by_file(&mut self, pom: &Path) -> Option<Rc<Model>> {
        let key = pom.display().to_string();
        if let Some(m) = self.models.get(&key) {
            return m.clone();
        }
        let m = self.load(pom, pom.parent());
        self.models.insert(key, m.clone());
        m
    }

    /// pom を読んで親から継承し、置き換える。`dir` はプロジェクトの中の pom（relativePath と basedir に使う）
    fn load(&mut self, pom: &Path, dir: Option<&Path>) -> Option<Rc<Model>> {
        let key = pom.display().to_string();
        if !self.busy.insert(key.clone()) {
            return None;
        }
        let m = (|| {
            let x = parse_xml(&std::fs::read_to_string(pom).ok()?)?;
            Some(Rc::new(self.build(&x, dir)))
        })();
        self.busy.remove(&key);
        m
    }

    fn profile_active(&self, act: &Xml, dir: Option<&Path>) -> Option<bool> {
        let mut any = false;
        let mut ok = true;
        if let Some(p) = act.get("property") {
            any = true;
            let name = p.s("name");
            let value = p.s("value");
            ok &= match name.strip_prefix('!') {
                Some(n) => !self.user.contains_key(n),
                None => self.user.get(&name).is_some_and(|v| value.is_empty() || *v == value),
            };
        }
        if let Some(j) = act.get("jdk") {
            any = true;
            let j = j.text.trim();
            let java = if self.java <= 8 { format!("1.{}", self.java) } else { self.java.to_string() };
            ok &= match j.strip_prefix('!') {
                Some(n) => !java.starts_with(n),
                None if j.starts_with(['[', '(']) => in_range(j, &java),
                None => java.starts_with(j),
            };
        }
        if let Some(os) = act.get("os") {
            any = true;
            let fam = os.s("family").to_ascii_lowercase();
            let name = os.s("name").to_ascii_lowercase();
            ok &= match fam.strip_prefix('!') {
                Some(f) => f != "windows",
                None => fam.is_empty() || fam == "windows",
            } && (name.is_empty() || name.starts_with("windows"));
        }
        if let Some(f) = act.get("file") {
            any = true;
            let base = dir.map(|d| d.display().to_string()).unwrap_or_default();
            let p = |s: String| PathBuf::from(s.replace("${basedir}", &base).replace("${project.basedir}", &base));
            let (e, m) = (f.s("exists"), f.s("missing"));
            ok &= (e.is_empty() || p(e).exists()) && (m.is_empty() || !p(m).exists());
        }
        any.then_some(ok)
    }

    fn build(&mut self, x: &Xml, dir: Option<&Path>) -> Model {
        // 親
        let mut layer = Layer::default();
        if let Some(p) = x.get("parent") {
            let (g, a) = (p.s("groupId"), p.s("artifactId"));
            let v = interp(&p.s("version"), &self.user, &HashMap::new());
            let rel = p.get("relativePath").map(|r| r.text.trim().to_string()).unwrap_or_else(|| "../pom.xml".into());
            let mut from_dir = None;
            if let (Some(d), false) = (dir, rel.is_empty()) {
                let f = d.join(&rel);
                let f = if f.is_dir() { f.join("pom.xml") } else { f };
                if f.is_file() {
                    // 親の座標が合うときだけ
                    let same = std::fs::read_to_string(&f).ok().and_then(|t| parse_xml(&t)).is_some_and(|px| {
                        let pg = px.s("groupId");
                        let pg = if pg.is_empty() { px.get("parent").map(|q| q.s("groupId")).unwrap_or_default() } else { pg };
                        px.s("artifactId") == a && pg == g
                    });
                    if same {
                        from_dir = self.by_file(&f);
                    }
                }
            }
            layer.parent = Some(match from_dir.or_else(|| self.by_gav(&g, &a, &v)) {
                Some(pm) => pm.layer.clone(),
                None => Rc::new(Layer { group: g.clone(), version: v.clone(), ..Default::default() }),
            });
            layer.parent_gav = Some((g, a, v));
        }
        layer.group = x.s("groupId");
        layer.artifact = x.s("artifactId");
        layer.version = x.s("version");
        layer.packaging = x.s("packaging");
        let mut modules: Vec<String> = Vec::new();
        let add = |l: &mut Layer, modules: &mut Vec<String>, p: &Xml| {
            if let Some(props) = p.get("properties") {
                for k in &props.kids {
                    l.props.retain(|(n, _)| *n != k.name);
                    l.props.push((k.name.clone(), k.text.trim().to_string()));
                }
            }
            let deps: Vec<Dep> = p.list("dependencies", "dependency").map(Dep::read).collect();
            merge_deps(&mut l.deps, &deps);
            let managed: Vec<Dep> = p
                .get("dependencyManagement")
                .into_iter()
                .flat_map(|d| d.list("dependencies", "dependency"))
                .map(Dep::read)
                .collect();
            merge_deps(&mut l.managed, &managed);
            modules.extend(p.list("modules", "module").map(|m| m.text.trim().to_string()));
        };
        add(&mut layer, &mut modules, x);
        // プロファイル: 条件の合うもの。どれも合わなければ activeByDefault
        let profiles: Vec<&Xml> = x.list("profiles", "profile").collect();
        let active: Vec<&Xml> = profiles
            .iter()
            .copied()
            .filter(|p| p.get("activation").and_then(|a| self.profile_active(a, dir)) == Some(true))
            .collect();
        let active = if active.is_empty() {
            profiles
                .iter()
                .copied()
                .filter(|p| p.get("activation").is_some_and(|a| a.s("activeByDefault") == "true"))
                .collect()
        } else {
            active
        };
        for p in active {
            add(&mut layer, &mut modules, p);
        }
        self.finish(Rc::new(layer), modules, dir)
    }

    /// モデルのプロパティ（親から継承し、project.* などを足して、値の中の ${} も置き換えた）
    fn props(&self, layer: &Layer, dir: Option<&Path>) -> HashMap<String, String> {
        let mut props: HashMap<String, String> = HashMap::new();
        for l in layer.chain() {
            for (k, v) in &l.props {
                props.insert(k.clone(), v.clone());
            }
        }
        let base = dir.map(|d| d.display().to_string()).unwrap_or_default();
        let packaging = if layer.packaging.is_empty() { "jar".to_string() } else { layer.packaging.clone() };
        for (k, v) in [
            ("project.groupId", layer.inherited(|l| &l.group)),
            ("project.artifactId", layer.artifact.clone()),
            ("project.version", layer.inherited(|l| &l.version)),
            ("project.packaging", packaging),
            ("project.basedir", base.clone()),
            ("basedir", base),
        ] {
            if let Some(p) = k.strip_prefix("project.") {
                props.insert(format!("pom.{p}"), v.clone());
                props.entry(p.to_string()).or_insert_with(|| v.clone());
            }
            props.insert(k.into(), v);
        }
        if let Some((g, a, v)) = &layer.parent_gav {
            props.insert("project.parent.groupId".into(), g.clone());
            props.insert("project.parent.artifactId".into(), a.clone());
            props.insert("project.parent.version".into(), v.clone());
        }
        // 値の中の ${} を先に（project.version が ${revision} など）
        let keys: Vec<String> = props.iter().filter(|(_, v)| v.contains("${")).map(|(k, _)| k.clone()).collect();
        for _ in 0..3 {
            for k in &keys {
                let v = &props[k];
                if v.contains("${") {
                    let v = interp(v, &self.user, &props);
                    props.insert(k.clone(), v);
                }
            }
        }
        props
    }

    fn finish(&mut self, layer: Rc<Layer>, modules: Vec<String>, dir: Option<&Path>) -> Model {
        let props = self.props(&layer, dir);
        let user = self.user.clone();
        let f = |s: &str| interp(s, &user, &props);
        let chain = layer.chain();
        let mut raw = Vec::new();
        for l in &chain {
            merge_deps(&mut raw, &l.deps);
        }
        let mut index = None;
        let mut deps = Vec::new();
        for d in &raw {
            let mut d = d.clone();
            d.map(&f);
            let idx = index.get_or_insert_with(|| ManagedIndex::new(&chain, &f));
            if let Some(m) = self.lookup(idx, &d.key(), &f) {
                apply_managed(&mut d, &m);
            }
            deps.push(d);
        }
        Model { group: f(&layer.inherited(|l| &l.group)), artifact: f(&layer.artifact), deps, modules, dir: dir.map(Path::to_path_buf), managed: Default::default(), layer: layer.clone() }
    }

    /// 管理の 1 つ（明示したもの、無ければ import の BOM を順に）
    fn lookup(&mut self, idx: &mut ManagedIndex, key: &str, f: &dyn Fn(&str) -> String) -> Option<Dep> {
        if let Some(d) = idx.explicit.get(key) {
            let mut d = (*d).clone();
            d.map(f);
            return Some(d);
        }
        if idx.boms.is_none() {
            let mut boms = Vec::new();
            for b in &idx.imports {
                if let Some(bom) = self.by_gav(&b.group, &b.artifact, &b.version) {
                    boms.push(self.managed(&bom));
                }
            }
            idx.boms = Some(boms);
        }
        idx.boms.iter().flatten().find_map(|m| m.get(key).cloned())
    }

    /// BOM の管理の全体（明示したものが勝ち、import は先に書いた BOM が勝つ）
    fn managed(&mut self, m: &Model) -> Rc<HashMap<String, Dep>> {
        if let Some(x) = m.managed.get() {
            return x.clone();
        }
        // import が巡っていたら、巡った先は空に
        let id = m as *const Model as usize;
        if !self.managing.insert(id) {
            return Rc::default();
        }
        let props = self.props(&m.layer, m.dir.as_deref());
        let user = self.user.clone();
        let f = |s: &str| interp(s, &user, &props);
        let layer = m.layer.clone();
        let chain = layer.chain();
        let idx = ManagedIndex::new(&chain, &f);
        let mut map: HashMap<String, Dep> = idx
            .explicit
            .iter()
            .map(|(k, d)| {
                let mut d = (*d).clone();
                d.map(&f);
                (k.clone(), d)
            })
            .collect();
        for b in &idx.imports {
            let Some(bom) = self.by_gav(&b.group, &b.artifact, &b.version) else { continue };
            for (k, d) in self.managed(&bom).iter() {
                map.entry(k.clone()).or_insert_with(|| d.clone());
            }
        }
        self.managing.remove(&id);
        let map = Rc::new(map);
        let _ = m.managed.set(map.clone());
        map
    }
}

/// `${...}` を置き換える（-D、モデルのプロパティ、env.X の順。無ければそのまま）
fn interp(s: &str, user: &HashMap<String, String>, props: &HashMap<String, String>) -> String {
    let mut s = s.to_string();
    for _ in 0..10 {
        if !s.contains("${") {
            break;
        }
        let mut out = String::new();
        let mut rest = s.as_str();
        let mut changed = false;
        while let Some(i) = rest.find("${") {
            out.push_str(&rest[..i]);
            let Some(j) = rest[i..].find('}') else {
                out.push_str(&rest[i..]);
                rest = "";
                break;
            };
            let name = &rest[i + 2..i + j];
            let v = user
                .get(name)
                .or_else(|| props.get(name))
                .cloned()
                .or_else(|| name.strip_prefix("env.").and_then(|e| std::env::var(e).ok()));
            match v {
                Some(v) => {
                    out.push_str(&v);
                    changed = true;
                }
                None => out.push_str(&rest[i..=i + j]),
            }
            rest = &rest[i + j + 1..];
        }
        out.push_str(rest);
        s = out;
        if !changed {
            break;
        }
    }
    s
}

fn apply_managed(d: &mut Dep, m: &Dep) {
    if d.version.is_empty() {
        d.version = m.version.clone();
    }
    if d.scope.is_empty() {
        d.scope = m.scope.clone();
    }
    if d.system_path.is_empty() {
        d.system_path = m.system_path.clone();
    }
    for e in &m.exclusions {
        if !d.exclusions.contains(e) {
            d.exclusions.push(e.clone());
        }
    }
}

fn excluded(ex: &[(String, String)], d: &Dep) -> bool {
    ex.iter().any(|(g, a)| (g == "*" || *g == d.group) && (a == "*" || *a == d.artifact))
}

impl Resolver {
    /// `root/pom.xml` とそのモジュールを読み、全モジュールの依存（全スコープ）の jar を合わせる
    pub fn resolve(mut self, root: &Path) -> Result<Resolved, String> {
        // reactor（モジュールの group:artifact → pom.xml）を先に集める
        let mut poms = Vec::new();
        let mut stack = vec![root.join("pom.xml")];
        let mut seen = HashSet::new();
        while let Some(pom) = stack.pop() {
            if !seen.insert(pom.display().to_string().to_lowercase()) {
                continue;
            }
            let Some(m) = self.by_file(&pom) else {
                if poms.is_empty() {
                    return Err(format!("{} を読めません", pom.display()));
                }
                continue;
            };
            self.reactor.insert(format!("{}:{}", m.group, m.artifact), pom.clone());
            let dir = pom.parent().unwrap_or(root).to_path_buf();
            for name in m.modules.iter().rev() {
                let p = dir.join(name);
                stack.push(if p.is_dir() { p.join("pom.xml") } else { p });
            }
            poms.push(pom);
        }
        // reactor を知ったので読み直す（モジュールどうしの親を手元のファイルから）
        self.models.clear();
        let mut out = Resolved { modules: poms.len(), ..Default::default() };
        let mut have = HashSet::new();
        for pom in &poms {
            let Some(m) = self.by_file(pom) else { continue };
            for jar in self.classpath(&m) {
                if have.insert(jar.clone()) {
                    out.jars.push(jar);
                }
            }
        }
        out.poms = self.models.len();
        out.missing = std::mem::take(&mut self.missing);
        out.missing.sort();
        out.missing.dedup();
        Ok(out)
    }

    /// 1 つのモジュールの依存の jar（幅優先。近い方・先に書いた方が勝つ）
    fn classpath(&mut self, m: &Model) -> Vec<PathBuf> {
        // 根の管理は、出てきた依存の分だけ引く（全体を作るとモジュールごとに 1 MB ほど）
        let layer = m.layer.clone();
        let props = self.props(&layer, m.dir.as_deref());
        let user = self.user.clone();
        let f = |s: &str| interp(s, &user, &props);
        let chain = layer.chain();
        let mut idx = ManagedIndex::new(&chain, &f);
        let mut managed: HashMap<String, Option<Dep>> = HashMap::new();
        let mut queue: VecDeque<(Dep, Rc<Vec<(String, String)>>)> = m.deps.iter().map(|d| (d.clone(), Rc::new(d.exclusions.clone()))).collect();
        let mut chosen = HashSet::new();
        let mut jars = Vec::new();
        while let Some((d, ex)) = queue.pop_front() {
            if !chosen.insert(d.key()) {
                continue;
            }
            let reactor = self.reactor.contains_key(&format!("{}:{}", d.group, d.artifact));
            let version = self.pick_version(&d.group, &d.artifact, &d.version);
            if !reactor {
                let file = if d.scope == "system" {
                    (!d.system_path.is_empty()).then(|| PathBuf::from(&d.system_path))
                } else {
                    match d.typ() {
                        "pom" => None,
                        "test-jar" => Some(self.artifact_file(
                            &d.group,
                            &d.artifact,
                            &version,
                            if d.classifier.is_empty() { "tests" } else { &d.classifier },
                            "jar",
                        )),
                        "jar" | "bundle" | "maven-plugin" | "ejb" => Some(self.artifact_file(&d.group, &d.artifact, &version, &d.classifier, "jar")),
                        _ => None,
                    }
                };
                if let Some(f) = file {
                    // モジュールごとに同じ jar を見るので、有る無しを覚えておく
                    let there = match self.files.get(&f) {
                        Some(&b) => b,
                        None => {
                            let b = f.is_file();
                            self.files.insert(f.clone(), b);
                            b
                        }
                    };
                    if there {
                        jars.push(f);
                    } else {
                        self.missing.push(format!("{}:{}:{version} (jar)", d.group, d.artifact));
                    }
                }
            }
            if d.scope == "system" {
                continue;
            }
            let Some(child) = self.by_gav(&d.group, &d.artifact, &version) else {
                continue;
            };
            for c in &child.deps {
                if matches!(c.scope.as_str(), "test" | "provided" | "system") || c.optional || excluded(&ex, c) {
                    continue;
                }
                let mut c = c.clone();
                let k = c.key();
                if !managed.contains_key(&k) {
                    let mg = self.lookup(&mut idx, &k, &f);
                    managed.insert(k.clone(), mg);
                }
                if let Some(mg) = &managed[&k] {
                    if !mg.version.is_empty() {
                        c.version = mg.version.clone();
                    }
                    for e in &mg.exclusions {
                        if !c.exclusions.contains(e) {
                            c.exclusions.push(e.clone());
                        }
                    }
                }
                let ex = if c.exclusions.is_empty() {
                    ex.clone()
                } else {
                    let mut v = (*ex).clone();
                    v.extend(c.exclusions.iter().cloned());
                    Rc::new(v)
                };
                queue.push_back((c, ex));
            }
        }
        jars
    }
}

/// `root` の Maven のプロジェクトの依存の jar を、手元のリポジトリから求める
pub fn resolve(root: &Path, java: u32) -> Result<Resolved, String> {
    let repo = local_repo().ok_or("ホームのフォルダーがわかりません")?;
    Resolver::new(repo, root, java).resolve(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        use std::cmp::Ordering::*;
        assert_eq!(version_cmp("1.10", "1.9"), Greater);
        assert_eq!(version_cmp("1.0", "1"), Equal);
        assert_eq!(version_cmp("1.0-SNAPSHOT", "1.0"), Less);
        assert_eq!(version_cmp("1.0-rc1", "1.0-beta2"), Greater);
        assert_eq!(version_cmp("2.0.Final", "2.0"), Equal);
        assert!(in_range("[1.0,2.0)", "1.5"));
        assert!(!in_range("[1.0,2.0)", "2.0"));
        assert!(in_range("(,1.0],[1.2,)", "1.3"));
        assert!(!in_range("(,1.0],[1.2,)", "1.1"));
        assert!(in_range("[1.5]", "1.5"));
    }

    #[test]
    fn xml() {
        let x = parse_xml("<?xml version=\"1.0\"?>\n<!-- c --><project a=\"b\"><x>1 &amp; 2</x><e/><l><i>a</i><i><![CDATA[<b>]]></i></l></project>").unwrap();
        assert_eq!(x.name, "project");
        assert_eq!(x.s("x"), "1 & 2");
        assert!(x.get("e").is_some());
        assert_eq!(x.list("l", "i").map(|i| i.text.clone()).collect::<Vec<_>>(), vec!["a", "<b>"]);
    }

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn pom(g: &str, a: &str, v: &str, body: &str) -> String {
        format!("<project><groupId>{g}</groupId><artifactId>{a}</artifactId><version>{v}</version>{body}</project>")
    }

    fn dep(g: &str, a: &str, v: &str, extra: &str) -> String {
        format!("<dependency><groupId>{g}</groupId><artifactId>{a}</artifactId><version>{v}</version>{extra}</dependency>")
    }

    /// 手元のリポジトリに pom と jar を置く
    fn publish(repo: &Path, g: &str, a: &str, v: &str, body: &str) {
        let d = format!("{}/{a}/{v}", g.replace('.', "/"));
        write(repo, &format!("{d}/{a}-{v}.pom"), &pom(g, a, v, body));
        write(repo, &format!("{d}/{a}-{v}.jar"), "");
    }

    #[test]
    fn resolves_like_maven() {
        let tmp = std::env::temp_dir().join(format!("nagi-m2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let repo = tmp.join("repo");
        let proj = tmp.join("proj");
        // 親（リポジトリ）: プロパティと管理と BOM
        publish(
            &repo,
            "p",
            "parent",
            "1",
            &format!(
                "<packaging>pom</packaging><properties><b.version>2</b.version></properties><dependencyManagement><dependencies>{}{}</dependencies></dependencyManagement>",
                dep("x", "b", "${b.version}", ""),
                dep("x", "bom", "1", "<type>pom</type><scope>import</scope>"),
            ),
        );
        publish(
            &repo,
            "x",
            "bom",
            "1",
            &format!(
                "<packaging>pom</packaging><dependencyManagement><dependencies>{}{}</dependencies></dependencyManagement>",
                dep("x", "c", "3", ""),
                dep("x", "b", "9", "")
            ),
        );
        // a → b(1), c(1), d(optional), t(test)。b は根の管理で 2、c は BOM で 3 に
        publish(
            &repo,
            "x",
            "a",
            "1",
            &format!(
                "<dependencies>{}{}{}{}</dependencies>",
                dep("x", "b", "1", ""),
                dep("x", "c", "1", ""),
                dep("x", "d", "1", "<optional>true</optional>"),
                dep("x", "t", "1", "<scope>test</scope>")
            ),
        );
        publish(&repo, "x", "b", "1", "");
        publish(&repo, "x", "b", "2", &format!("<dependencies>{}</dependencies>", dep("x", "e", "1", "")));
        publish(&repo, "x", "c", "3", &format!("<dependencies>{}</dependencies>", dep("x", "f", "1", "")));
        publish(&repo, "x", "e", "1", "");
        publish(&repo, "x", "e", "2", "");
        publish(&repo, "x", "f", "1", "");
        // プロジェクト: 根（親は p:parent）と、モジュール m（親は根。${revision} は maven.config から）
        write(
            &proj,
            "pom.xml",
            &format!(
                "<project><parent><groupId>p</groupId><artifactId>parent</artifactId><version>1</version><relativePath/></parent><groupId>g</groupId><artifactId>root</artifactId><version>${{revision}}</version><packaging>pom</packaging><modules><module>m</module><module>n</module></modules></project>"
            ),
        );
        write(&proj, ".mvn/maven.config", "-Drevision=7");
        write(
            &proj,
            "m/pom.xml",
            &format!(
                "<project><parent><groupId>g</groupId><artifactId>root</artifactId><version>${{revision}}</version></parent><artifactId>m</artifactId><dependencies>{}{}</dependencies></project>",
                dep(
                    "x",
                    "a",
                    "1",
                    "<exclusions><exclusion><groupId>x</groupId><artifactId>f</artifactId></exclusion></exclusions>"
                ),
                dep("g", "n", "${project.version}", ""),
            ),
        );
        // n はモジュール（jar は無いが、その依存は入る）。e は直接 2 を書いたので近い方が勝つ
        write(
            &proj,
            "n/pom.xml",
            &format!(
                "<project><parent><groupId>g</groupId><artifactId>root</artifactId><version>${{revision}}</version></parent><artifactId>n</artifactId><dependencies>{}</dependencies></project>",
                dep("x", "e", "2", ""),
            ),
        );
        let r = Resolver::new(repo.clone(), &proj, 17).resolve(&proj).unwrap();
        let names: Vec<String> = r
            .jars
            .iter()
            .map(|p| p.strip_prefix(&repo).unwrap().file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(r.missing, Vec::<String>::new());
        assert_eq!(r.modules, 3);
        // m: a, (n はモジュール), b-2, c-3, e-2（n から深さ 2。b-2 の e-1 は深さ 3）。f は除外。n: e-2
        assert_eq!(names, vec!["a-1.jar", "b-2.jar", "c-3.jar", "e-2.jar"]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 実際の Maven のプロジェクトで、mvn の結果（NAGI_MAVEN_CP の 2 行目から）と比べる
    /// （NAGI_MAVEN_ROOT=… NAGI_MAVEN_CP=… cargo test -p nagi-workers maven_deps_real -- --ignored --nocapture）
    #[test]
    #[ignore]
    fn maven_deps_real() {
        let Some(root) = std::env::var_os("NAGI_MAVEN_ROOT") else {
            return;
        };
        let before = crate::sys::private_peak();
        for _ in 0..2 {
            let t = std::time::Instant::now();
            let r = resolve(Path::new(&root), 17).unwrap();
            println!(
                "{} modules, {} poms, {} jars, {} ms, missing {:?}",
                r.modules,
                r.poms,
                r.jars.len(),
                t.elapsed().as_millis(),
                r.missing
            );
        }
        let after = crate::sys::private_peak();
        println!(
            "private {} -> {} MB, peak {} -> {} MB",
            before.0 >> 20,
            after.0 >> 20,
            before.1 >> 20,
            after.1 >> 20
        );
        let r = resolve(Path::new(&root), 17).unwrap();
        let Some(cp) = std::env::var_os("NAGI_MAVEN_CP") else {
            return;
        };
        let text = std::fs::read_to_string(cp).unwrap();
        let want: HashSet<String> = text.lines().skip(1).map(|l| l.trim().to_lowercase()).collect();
        let got: HashSet<String> = r.jars.iter().map(|p| p.display().to_string().to_lowercase()).collect();
        let mut extra: Vec<_> = got.difference(&want).collect();
        let mut lack: Vec<_> = want.difference(&got).collect();
        extra.sort();
        lack.sort();
        println!("mvn {} / nagi {}\nextra {extra:#?}\nlack {lack:#?}", want.len(), got.len());
    }
}
