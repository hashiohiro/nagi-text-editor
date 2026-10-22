//! Java のライブラリの型の索引（jar・JDK の class ファイルを読む。LSP もコンパイラも使わない）。
//!
//! jar は中身が変わらない（`.m2` の jar は版ごとに別のファイル）ので、jar 1 つごとに索引のファイル（かけら）を作り、
//! jar のパス・大きさ・更新時刻で使い回す。1 つの class ずつ読んでは捨てるので、作る間のメモリは jar 1 つ分の文字に収まる。
//!
//! 残すもの: 公開しているクラス（入れ子・無名・合成を除く）と、その public・protected のフィールドとメソッド。
//! 型は記述子（`(Ljava/lang/String;)V`）と、ジェネリクスの情報（Signature 属性）をそのまま持つ。
//!
//! かけらの形（リトルエンディアン）:
//! `NGJ0` | 文字の数 n | 文字の終わりの位置 u32 × n | 文字のバイト列 | クラスの数 m | クラス m 個 | 単純名で並べた (文字番号, クラスの位置) × m
//! クラス: 名前・親・Signature（文字番号。無ければ u32::MAX）・アクセス u16・インターフェースの数 u16 と文字番号・
//! フィールドの数 u16 と (名前, 記述子, Signature, アクセス u16)・メソッドの数 u16 と同じ形

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const NONE: u32 = u32::MAX;

/// class ファイルから読んだメンバー
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub desc: String,
    pub signature: Option<String>,
    pub access: u16,
}

/// class ファイルから読んだクラス
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Class {
    /// 内部名（`java/util/List`）
    pub name: String,
    pub super_name: Option<String>,
    pub interfaces: Vec<String>,
    pub signature: Option<String>,
    pub access: u16,
    pub fields: Vec<Member>,
    pub methods: Vec<Member>,
}

pub const ACC_PUBLIC: u16 = 0x0001;
pub const ACC_PRIVATE: u16 = 0x0002;
pub const ACC_PROTECTED: u16 = 0x0004;
pub const ACC_STATIC: u16 = 0x0008;
const ACC_BRIDGE: u16 = 0x0040;
const ACC_SYNTHETIC: u16 = 0x1000;

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Option<u8> {
        let v = *self.b.get(self.i)?;
        self.i += 1;
        Some(v)
    }
    fn u16(&mut self) -> Option<u16> {
        let s = self.b.get(self.i..self.i + 2)?;
        self.i += 2;
        Some(u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        let s = self.b.get(self.i..self.i + 4)?;
        self.i += 4;
        Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.i..self.i + n)?;
        self.i += n;
        Some(s)
    }
}

/// class ファイル（ct.sym の .sig も同じ形）を読む。読めなければ None
pub fn parse_class(b: &[u8]) -> Option<Class> {
    let mut r = Reader { b, i: 0 };
    if r.u32()? != 0xCAFEBABE {
        return None;
    }
    r.u32()?;
    let n = r.u16()? as usize;
    // 定数: 文字（Utf8）と、クラス（文字の番号）だけを覚える
    let mut utf: Vec<Option<&[u8]>> = vec![None; n];
    let mut class_ref: Vec<u16> = vec![0; n];
    let mut k = 1;
    while k < n {
        let tag = r.u8()?;
        match tag {
            1 => {
                let len = r.u16()? as usize;
                utf[k] = Some(r.bytes(len)?);
            }
            7 => class_ref[k] = r.u16()?,
            8 | 16 | 19 | 20 => {
                r.u16()?;
            }
            3 | 4 | 9 | 10 | 11 | 12 | 17 | 18 => {
                r.u32()?;
            }
            15 => {
                r.bytes(3)?;
            }
            5 | 6 => {
                r.bytes(8)?;
                k += 1;
            }
            _ => return None,
        }
        k += 1;
    }
    let s = |i: u16| -> Option<String> { utf.get(i as usize).copied().flatten().map(|b| String::from_utf8_lossy(b).into_owned()) };
    let cls = |i: u16| -> Option<String> { if i == 0 { None } else { s(*class_ref.get(i as usize)?) } };
    let access = r.u16()?;
    let name = cls(r.u16()?)?;
    let super_name = cls(r.u16()?);
    let ni = r.u16()?;
    let mut interfaces = Vec::with_capacity(ni as usize);
    for _ in 0..ni {
        interfaces.extend(cls(r.u16()?));
    }
    // 属性の並びから Signature を探す（ほかは飛ばす）
    let attrs = |r: &mut Reader| -> Option<Option<String>> {
        let mut sig = None;
        for _ in 0..r.u16()? {
            let an = r.u16()?;
            let len = r.u32()? as usize;
            let body = r.bytes(len)?;
            if utf.get(an as usize).copied().flatten() == Some(b"Signature".as_slice()) && body.len() == 2 {
                sig = s(u16::from_be_bytes([body[0], body[1]]));
            }
        }
        Some(sig)
    };
    let members = |r: &mut Reader| -> Option<Vec<Member>> {
        let n = r.u16()?;
        let mut v = Vec::new();
        for _ in 0..n {
            let access = r.u16()?;
            let name = s(r.u16()?)?;
            let desc = s(r.u16()?)?;
            let signature = attrs(r)?;
            if access & (ACC_PUBLIC | ACC_PROTECTED) != 0 && access & (ACC_SYNTHETIC | ACC_BRIDGE) == 0 {
                v.push(Member { name, desc, signature, access });
            }
        }
        Some(v)
    };
    let fields = members(&mut r)?;
    let methods = members(&mut r)?;
    let signature = attrs(&mut r)?;
    Some(Class { name, super_name, interfaces, signature, access, fields, methods })
}

/// 索引に入れるクラスか（公開していて、無名・合成でない。入れ子は `Map$Entry` のように名前のあるものだけ）
fn wanted(c: &Class) -> bool {
    c.access & ACC_PUBLIC != 0
        && c.access & ACC_SYNTHETIC == 0
        && !c.name.ends_with("module-info")
        && !c.name.ends_with("package-info")
        && !c.name.rsplit('$').next().is_some_and(|t| t.starts_with(|ch: char| ch.is_ascii_digit()))
}

/// zip の中の 1 つ（中央ディレクトリから）
struct Entry {
    name: String,
    method: u16,
    csize: u64,
    offset: u64,
}

/// zip の中央ディレクトリを読む（ZIP64 は扱わない）
fn zip_entries(f: &mut File) -> std::io::Result<Vec<Entry>> {
    let len = f.metadata()?.len();
    let tail = len.min(65_557);
    f.seek(SeekFrom::Start(len - tail))?;
    let mut buf = vec![0; tail as usize];
    f.read_exact(&mut buf)?;
    let eocd = (0..buf.len().saturating_sub(21)).rev().find(|&i| buf[i..i + 4] == [0x50, 0x4b, 0x05, 0x06]).ok_or_else(|| std::io::Error::other("zip ではありません"))?;
    let le16 = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let le32 = |b: &[u8], i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let count = le16(&buf, eocd + 10) as usize;
    let size = le32(&buf, eocd + 12) as usize;
    let start = le32(&buf, eocd + 16) as u64;
    f.seek(SeekFrom::Start(start))?;
    let mut cd = vec![0; size];
    f.read_exact(&mut cd)?;
    let mut out = Vec::with_capacity(count);
    let mut i = 0;
    while i + 46 <= cd.len() && cd[i..i + 4] == [0x50, 0x4b, 0x01, 0x02] {
        let nlen = le16(&cd, i + 28) as usize;
        let xlen = le16(&cd, i + 30) as usize;
        let clen = le16(&cd, i + 32) as usize;
        out.push(Entry {
            name: String::from_utf8_lossy(&cd[i + 46..i + 46 + nlen]).into_owned(),
            method: le16(&cd, i + 10),
            csize: le32(&cd, i + 20) as u64,
            offset: le32(&cd, i + 42) as u64,
        });
        i += 46 + nlen + xlen + clen;
    }
    Ok(out)
}

/// zip の 1 つを展開する（無圧縮か DEFLATE）
fn zip_read(f: &mut File, e: &Entry, comp: &mut Vec<u8>) -> Option<Vec<u8>> {
    let mut h = [0u8; 30];
    f.seek(SeekFrom::Start(e.offset)).ok()?;
    f.read_exact(&mut h).ok()?;
    let skip = u16::from_le_bytes([h[26], h[27]]) as i64 + u16::from_le_bytes([h[28], h[29]]) as i64;
    f.seek(SeekFrom::Current(skip)).ok()?;
    comp.resize(e.csize as usize, 0);
    f.read_exact(comp).ok()?;
    match e.method {
        0 => Some(comp.clone()),
        8 => miniz_oxide::inflate::decompress_to_vec(comp).ok(),
        _ => None,
    }
}

/// jar（ct.sym も）の公開しているクラスを、1 つずつ `each` に渡す。`keep` は中の名前で読むものを選ぶ
pub fn read_jar(path: &Path, keep: impl Fn(&str) -> bool, mut each: impl FnMut(Class)) -> std::io::Result<usize> {
    let mut f = File::open(path)?;
    let entries = zip_entries(&mut f)?;
    let mut comp = Vec::new();
    let mut n = 0;
    for e in entries.iter().filter(|e| (e.name.ends_with(".class") || e.name.ends_with(".sig")) && !e.name.starts_with("META-INF/") && keep(&e.name)) {
        let Some(bytes) = zip_read(&mut f, e, &mut comp) else { continue };
        if let Some(c) = parse_class(&bytes).filter(wanted) {
            n += 1;
            each(c);
        }
    }
    Ok(n)
}

/// かけらを書く
struct ShardWriter {
    strings: Vec<u8>,
    ends: Vec<u32>,
    ids: HashMap<String, u32>,
    classes: Vec<u8>,
    /// (単純名の文字番号, クラスの位置)
    by_simple: Vec<(u32, u32)>,
}

impl ShardWriter {
    fn new() -> Self {
        ShardWriter { strings: Vec::new(), ends: Vec::new(), ids: HashMap::new(), classes: Vec::new(), by_simple: Vec::new() }
    }

    fn id(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.ids.get(s) {
            return i;
        }
        let i = self.ends.len() as u32;
        self.strings.extend_from_slice(s.as_bytes());
        self.ends.push(self.strings.len() as u32);
        self.ids.insert(s.to_string(), i);
        i
    }

    fn opt(&mut self, s: &Option<String>) -> u32 {
        s.as_deref().map_or(NONE, |s| self.id(s))
    }

    fn add(&mut self, c: &Class) {
        let at = self.classes.len() as u32;
        let simple = c.name.rsplit(['/', '$']).next().unwrap_or(&c.name).to_string();
        let sid = self.id(&simple);
        self.by_simple.push((sid, at));
        let put32 = |w: &mut Self, v: u32| w.classes.extend_from_slice(&v.to_le_bytes());
        let v = self.id(&c.name);
        put32(self, v);
        let v = self.opt(&c.super_name);
        put32(self, v);
        let v = self.opt(&c.signature);
        put32(self, v);
        self.classes.extend_from_slice(&c.access.to_le_bytes());
        self.classes.extend_from_slice(&(c.interfaces.len() as u16).to_le_bytes());
        for i in &c.interfaces {
            let v = self.id(i);
            put32(self, v);
        }
        for list in [&c.fields, &c.methods] {
            self.classes.extend_from_slice(&(list.len() as u16).to_le_bytes());
            for m in list {
                let (a, b, s) = (self.id(&m.name), self.id(&m.desc), self.opt(&m.signature));
                put32(self, a);
                put32(self, b);
                put32(self, s);
                self.classes.extend_from_slice(&m.access.to_le_bytes());
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        let strings = std::mem::take(&mut self.strings);
        let name = |id: u32| {
            let a = if id == 0 { 0 } else { self.ends[id as usize - 1] as usize };
            &strings[a..self.ends[id as usize] as usize]
        };
        self.by_simple.sort_by(|x, y| name(x.0).cmp(name(y.0)));
        let mut out = Vec::with_capacity(16 + self.ends.len() * 4 + strings.len() + self.classes.len() + self.by_simple.len() * 8);
        out.extend_from_slice(b"NGJ0");
        out.extend_from_slice(&(self.ends.len() as u32).to_le_bytes());
        for e in &self.ends {
            out.extend_from_slice(&e.to_le_bytes());
        }
        out.extend_from_slice(&strings);
        out.extend_from_slice(&(self.by_simple.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.classes);
        for (s, at) in &self.by_simple {
            out.extend_from_slice(&s.to_le_bytes());
            out.extend_from_slice(&at.to_le_bytes());
        }
        out
    }
}

/// jar 1 つのかけらを作る（バイト列）。`keep` は中の名前で読むものを選ぶ
pub fn build_shard(jar: &Path, keep: impl Fn(&str) -> bool) -> std::io::Result<(Vec<u8>, usize)> {
    let mut w = ShardWriter::new();
    let n = read_jar(jar, keep, |c| w.add(&c))?;
    Ok((w.finish(), n))
}

/// 読むだけのかけら（メモリに写したバイト列の上で引く）
pub struct Shard<'a> {
    b: &'a [u8],
    ends: usize,
    nstr: usize,
    strings: usize,
    classes: usize,
    table: usize,
    nclass: usize,
}

impl<'a> Shard<'a> {
    pub fn open(b: &'a [u8]) -> Option<Shard<'a>> {
        if b.get(0..4)? != b"NGJ0" {
            return None;
        }
        let u32_at = |i: usize| -> Option<usize> { Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?) as usize) };
        let nstr = u32_at(4)?;
        let ends = 8;
        let strings = ends + nstr * 4;
        let slen = if nstr == 0 { 0 } else { u32_at(ends + (nstr - 1) * 4)? };
        let nclass = u32_at(strings + slen)?;
        let classes = strings + slen + 4;
        let table = b.len().checked_sub(nclass * 8)?;
        Some(Shard { b, ends, nstr, strings, classes, table, nclass })
    }

    fn u32_at(&self, i: usize) -> u32 {
        u32::from_le_bytes(self.b[i..i + 4].try_into().unwrap())
    }

    pub fn string(&self, id: u32) -> &'a str {
        if id == NONE || id as usize >= self.nstr {
            return "";
        }
        let a = if id == 0 { 0 } else { self.u32_at(self.ends + (id as usize - 1) * 4) as usize };
        let e = self.u32_at(self.ends + id as usize * 4) as usize;
        std::str::from_utf8(&self.b[self.strings + a..self.strings + e]).unwrap_or("")
    }

    pub fn len(&self) -> usize {
        self.nclass
    }

    pub fn is_empty(&self) -> bool {
        self.nclass == 0
    }

    /// 単純名（`List`・`Entry`）のクラスの内部名
    pub fn by_simple_name(&self, simple: &str) -> Vec<&'a str> {
        let key = |k: usize| self.string(self.u32_at(self.table + k * 8));
        let lo = partition(self.nclass, |k| key(k) < simple);
        (lo..self.nclass).take_while(|&k| key(k) == simple).map(|k| self.string(self.u32_at(self.classes + self.u32_at(self.table + k * 8 + 4) as usize))).collect()
    }

    /// クラスを読む（位置から）
    fn class_at(&self, at: usize) -> Class {
        let mut i = self.classes + at;
        let u32n = |i: &mut usize| {
            let v = self.u32_at(*i);
            *i += 4;
            v
        };
        let u16n = |i: &mut usize| {
            let v = u16::from_le_bytes([self.b[*i], self.b[*i + 1]]);
            *i += 2;
            v
        };
        let opt = |id: u32| (id != NONE).then(|| self.string(id).to_string());
        let name = self.string(u32n(&mut i)).to_string();
        let super_name = opt(u32n(&mut i));
        let signature = opt(u32n(&mut i));
        let access = u16n(&mut i);
        let interfaces = (0..u16n(&mut i)).map(|_| self.string(u32n(&mut i)).to_string()).collect();
        let mut lists = [Vec::new(), Vec::new()];
        for list in &mut lists {
            for _ in 0..u16n(&mut i) {
                let name = self.string(u32n(&mut i)).to_string();
                let desc = self.string(u32n(&mut i)).to_string();
                let signature = opt(u32n(&mut i));
                let access = u16n(&mut i);
                list.push(Member { name, desc, signature, access });
            }
        }
        let [fields, methods] = lists;
        Class { name, super_name, interfaces, signature, access, fields, methods }
    }

    /// 単純名の表の k 番目の単純名
    fn simple_at(&self, k: usize) -> &'a str {
        self.string(self.u32_at(self.table + k * 8))
    }

    /// 単純名の表の k 番目のクラスの内部名
    fn name_at(&self, k: usize) -> &'a str {
        self.string(self.u32_at(self.classes + self.u32_at(self.table + k * 8 + 4) as usize))
    }

    /// 単純名の表の k 番目のクラス
    fn class_at_table(&self, k: usize) -> Class {
        self.class_at(self.u32_at(self.table + k * 8 + 4) as usize)
    }

    /// 内部名でクラスを引く（単純名で絞ってから）
    pub fn class(&self, internal: &str) -> Option<Class> {
        let simple = internal.rsplit(['/', '$']).next().unwrap_or(internal);
        let key = |k: usize| self.string(self.u32_at(self.table + k * 8));
        let lo = partition(self.nclass, |k| key(k) < simple);
        (lo..self.nclass).take_while(|&k| key(k) == simple).map(|k| self.u32_at(self.table + k * 8 + 4) as usize).find(|&at| self.string(self.u32_at(self.classes + at)) == internal).map(|at| self.class_at(at))
    }
}

/// jar の索引を置く場所（`%LOCALAPPDATA%\\nagi\\index\\jars`。試験では NAGI_INDEX_DIR の下）
pub fn jars_dir() -> Option<PathBuf> {
    let base = std::env::var_os("NAGI_INDEX_DIR").map(PathBuf::from).or_else(|| std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("nagi").join("index")))?;
    Some(base.join("jars"))
}

/// jar のかけらのファイル名（jar のパス・大きさ・更新時刻から。jar が変われば別の名前）
fn shard_name(jar: &Path, tag: &str) -> Option<String> {
    let m = std::fs::metadata(jar).ok()?;
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let key = format!("{}|{}|{t}|{tag}", jar.display(), m.len());
    Some(format!("{}.ngj", &nagi_core::hash::sha256_hex(key.as_bytes())?[..24]))
}

/// 読む jar（`keep` は中の名前で読むものを選ぶ。ct.sym で版を選ぶときに使う。`tag` はその選び方の名前）
pub struct JarSource<'a> {
    pub path: &'a Path,
    pub keep: &'a dyn Fn(&str) -> bool,
    pub tag: &'a str,
}

/// クラスパス全体の型の索引。かけらはメモリに写して持ち、単純名の表だけをまとめて持つ（クラス 1 つに 8 バイト）
pub struct JarIndex {
    shards: Vec<std::sync::Arc<nagi_core::mmap::Mapped>>,
    /// 単純名で並べた (かけら, かけらの中の単純名の表の番号)
    table: Vec<(u16, u32)>,
    /// 作り直したかけらの数
    pub built: usize,
}

impl JarIndex {
    /// かけらを読み（無ければ作って `dir` に書き）、写して束ねる。作る間は jar 1 つ分ずつ
    pub fn load(sources: &[JarSource], dir: &Path) -> JarIndex {
        let _ = std::fs::create_dir_all(dir);
        let mut shards = Vec::new();
        let mut built = 0;
        for src in sources {
            let Some(name) = shard_name(src.path, src.tag) else { continue };
            let file = dir.join(name);
            if !file.is_file() {
                let Ok((bytes, _)) = build_shard(src.path, src.keep) else { continue };
                // 書きかけを読まないよう、別の名前で書いてから置き換える
                let tmp = file.with_extension(format!("tmp{}", std::process::id()));
                if std::fs::write(&tmp, &bytes).is_err() || std::fs::rename(&tmp, &file).is_err() {
                    let _ = std::fs::remove_file(&tmp);
                    continue;
                }
                built += 1;
            }
            if let Some(m) = nagi_core::mmap::Mapped::open(&file).filter(|m| Shard::open(m).is_some()) {
                shards.push(m);
            }
        }
        let mut table: Vec<(u16, u32)> = Vec::new();
        for (i, m) in shards.iter().enumerate() {
            let s = Shard::open(m).unwrap();
            table.extend((0..s.len() as u32).map(|k| (i as u16, k)));
        }
        let views: Vec<Shard> = shards.iter().map(|m| Shard::open(m).unwrap()).collect();
        table.sort_by(|a, b| views[a.0 as usize].simple_at(a.1 as usize).cmp(views[b.0 as usize].simple_at(b.1 as usize)));
        table.shrink_to_fit();
        drop(views);
        JarIndex { shards, table, built }
    }

    fn shard(&self, i: u16) -> Shard<'_> {
        Shard::open(&self.shards[i as usize]).unwrap()
    }

    pub fn classes(&self) -> usize {
        self.table.len()
    }

    /// 単純名（`List`）のクラスの内部名（`java/util/List`）。import の候補
    pub fn by_simple_name(&self, simple: &str) -> Vec<&str> {
        let key = |k: usize| self.shard(self.table[k].0).simple_at(self.table[k].1 as usize);
        let lo = partition(self.table.len(), |k| key(k) < simple);
        (lo..self.table.len()).take_while(|&k| key(k) == simple).map(|k| self.shard(self.table[k].0).name_at(self.table[k].1 as usize)).collect()
    }

    /// 内部名でクラスを引く
    pub fn class(&self, internal: &str) -> Option<Class> {
        let simple = internal.rsplit(['/', '$']).next().unwrap_or(internal);
        let key = |k: usize| self.shard(self.table[k].0).simple_at(self.table[k].1 as usize);
        let lo = partition(self.table.len(), |k| key(k) < simple);
        (lo..self.table.len()).take_while(|&k| key(k) == simple).find_map(|k| {
            let (i, at) = self.table[k];
            let s = self.shard(i);
            (s.name_at(at as usize) == internal).then(|| s.class_at_table(at as usize))
        })
    }
}

/// `pred` が true の並びの長さ（0..n の前から true が続く）
fn partition(n: usize, pred: impl Fn(usize) -> bool) -> usize {
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if pred(mid) { lo = mid + 1 } else { hi = mid }
    }
    lo
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 試験に使う JDK（JAVA_HOME か、見つけた JDK）
    fn jdk_home() -> Option<std::path::PathBuf> {
        std::env::var_os("JAVA_HOME").map(Into::into).or_else(|| crate::jdk::installed().into_iter().next().map(|j| j.path))
    }

    /// このプロセスの private のバイト数
    fn private_bytes() -> usize {
        #[repr(C)]
        #[derive(Default)]
        struct Counters {
            cb: u32,
            page_faults: u32,
            peak_ws: usize,
            ws: usize,
            qpp: usize,
            qp: usize,
            qnpp: usize,
            qnp: usize,
            pagefile: usize,
            peak_pagefile: usize,
            private: usize,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn K32GetProcessMemoryInfo(h: isize, c: *mut Counters, cb: u32) -> i32;
            fn GetCurrentProcess() -> isize;
        }
        let mut c = Counters { cb: std::mem::size_of::<Counters>() as u32, ..Default::default() };
        unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
        c.private
    }

    /// JDK の ct.sym から java.util.List を読み、かけらにして引き直す
    #[test]
    fn jdk_list_roundtrip() {
        let Some(home) = jdk_home() else { return };
        let ct = home.join("lib").join("ct.sym");
        if !ct.is_file() {
            return;
        }
        // 版の文字（ct.sym の上のフォルダー名）に G（16）を含むものだけ
        let keep = |n: &str| n.split('/').next().is_some_and(|d| d.contains('G')) && n.contains("/java.base/java/util/");
        let (bytes, n) = build_shard(&ct, keep).unwrap();
        assert!(n > 100, "{n}");
        let s = Shard::open(&bytes).unwrap();
        assert_eq!(s.by_simple_name("List"), vec!["java/util/List"]);
        let list = s.class("java/util/List").unwrap();
        assert_eq!(list.interfaces, vec!["java/util/Collection"]);
        assert_eq!(list.signature.as_deref(), Some("<E:Ljava/lang/Object;>Ljava/lang/Object;Ljava/util/Collection<TE;>;"));
        let get = list.methods.iter().find(|m| m.name == "get").unwrap();
        assert_eq!((get.desc.as_str(), get.signature.as_deref()), ("(I)Ljava/lang/Object;", Some("(I)TE;")));
        assert_eq!(s.by_simple_name("Entry"), vec!["java/util/Map$Entry"]);
    }

    /// 索引を作る時間・メモリ・大きさ（NAGI_JARS_FILE にクラスパス（; 区切り）を書いたファイルを入れて
    /// cargo test -p nagi-workers --release jar_index_cost -- --ignored --nocapture）。2 回目はかけらを写すだけ
    #[test]
    #[ignore]
    fn jar_index_cost() {
        let list = std::env::var("NAGI_JARS_FILE").ok().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
        let jars: Vec<PathBuf> = list.trim().split(';').filter(|s| s.ends_with(".jar")).map(Into::into).collect();
        let dir = std::env::temp_dir().join("nagi-jar-index-cost");
        if std::env::var_os("NAGI_FRESH").is_some() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        let ct = jdk_home().map(|h| h.join("lib").join("ct.sym"));
        let all = |_: &str| true;
        let jdk16 = |n: &str| n.split('/').next().is_some_and(|d| d.contains('G'));
        let mut sources: Vec<JarSource> = jars.iter().map(|p| JarSource { path: p, keep: &all, tag: "" }).collect();
        if let Some(ct) = &ct {
            sources.push(JarSource { path: ct, keep: &jdk16, tag: "G" });
        }
        let m0 = private_bytes();
        let t0 = std::time::Instant::now();
        let ix = JarIndex::load(&sources, &dir);
        let load = t0.elapsed();
        let m1 = private_bytes();
        let disk: u64 = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.metadata().map_or(0, |m| m.len())).sum();
        // 引く速さ: import の候補（単純名）と、クラスを取り出す
        let names = ["List", "String", "Map", "Optional", "Autowired", "Service", "Logger", "ResponseEntity", "Transactional", "LocalDate"];
        let t1 = std::time::Instant::now();
        let mut hits = 0;
        for _ in 0..1000 {
            for n in names {
                hits += ix.by_simple_name(n).len();
            }
        }
        let per_simple = t1.elapsed() / (1000 * names.len() as u32);
        let t2 = std::time::Instant::now();
        for _ in 0..1000 {
            assert!(ix.class("java/util/List").is_some());
        }
        let per_class = t2.elapsed() / 1000;
        let m2 = private_bytes();
        let mb = |b: usize| b as f64 / 1_048_576.0;
        println!(
            "{} jars + JDK → {} classes ({} shards built), on disk {:.1} MB, load {} ms, private {:.1} → {:.1} MB after load → {:.1} MB after lookups, by_simple_name {:?} ({} hits), class() {:?}",
            jars.len(), ix.classes(), ix.built, disk as f64 / 1_048_576.0, load.as_millis(), mb(m0), mb(m1), mb(m2), per_simple, hits / 1000, per_class
        );
    }
}
