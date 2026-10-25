//! Java のライブラリの型の索引（jar・JDK の class ファイルを読む。LSP もコンパイラも使わない）。
//!
//! jar は中身が変わらない（`.m2` の jar は版ごとに別のファイル）ので、jar 1 つごとに索引のファイル（かけら）を作り、
//! jar のパス・大きさ・更新時刻で使い回す。1 つの class ずつ読んでは捨てるので、作る間のメモリは jar 1 つ分の文字に収まる。
//!
//! 残すもの: 公開しているクラス（入れ子・無名・合成を除く）と、その public・protected のフィールドとメソッド。
//! 型は記述子（`(Ljava/lang/String;)V`）と、ジェネリクスの情報（Signature 属性）をそのまま持つ。
//!
//! かけらの形（リトルエンディアン。クラスは読んだ端から書くので、文字の表は後ろ）:
//! 頭 24 バイト（`NGJ1`・文字の数 n・クラスの数 m・文字の終わりの位置の場所・文字の場所・単純名の表の場所）| クラス m 個 |
//! 文字の終わりの位置 u32 × n | 文字のバイト列 | 単純名で並べた (文字番号, クラスの位置) × m
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

/// zip（jar・src.zip）の中で `pred` に合う最初のものの名前と中身
pub fn zip_find(path: &Path, pred: impl Fn(&str) -> bool) -> Option<(String, Vec<u8>)> {
    let mut f = File::open(path).ok()?;
    let entries = zip_entries(&mut f).ok()?;
    let e = entries.iter().find(|e| pred(&e.name))?;
    let mut comp = Vec::new();
    Some((e.name.clone(), zip_read(&mut f, e, &mut comp)?))
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

/// かけらを書く。クラスと文字は読んだ端からファイルへ流し、メモリに持つのは文字のハッシュと単純名の表だけ
/// （全部をメモリに組んでから書くと、55 MB の jar で 90 MB に達した。文字だけで 16 MB あり、伸ばすときに倍を取る）
struct ShardWriter {
    out: std::io::BufWriter<File>,
    /// 文字を流す一時ファイル（最後に out の後ろへ写す）
    strs: std::io::BufWriter<File>,
    strs_path: PathBuf,
    /// クラスの部分に書いたバイト数
    written: u32,
    /// 文字のバイト数（書いた分）
    str_len: u32,
    ends: Vec<u32>,
    /// 文字のハッシュ（SipHash の 64 ビット。14 万の文字で取り違える見込みは 10 億分の 1 未満）→ 文字番号
    ids: HashMap<u64, u32>,
    /// (単純名, クラスの位置)。並べるために単純名だけは持つ
    by_simple: Vec<(Box<str>, u32, u32)>,
    rec: Vec<u8>,
}

/// 文字のハッシュ
fn str_hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

const HEAD: usize = 24;

impl ShardWriter {
    fn create(path: &Path) -> std::io::Result<Self> {
        let mut out = std::io::BufWriter::with_capacity(64 * 1024, File::create(path)?);
        std::io::Write::write_all(&mut out, &[0u8; HEAD])?;
        let strs_path = path.with_extension("str");
        let strs = std::io::BufWriter::with_capacity(64 * 1024, File::create(&strs_path)?);
        Ok(ShardWriter { out, strs, strs_path, written: 0, str_len: 0, ends: Vec::new(), ids: HashMap::new(), by_simple: Vec::new(), rec: Vec::new() })
    }

    fn id(&mut self, s: &str) -> std::io::Result<u32> {
        let h = str_hash(s);
        if let Some(&i) = self.ids.get(&h) {
            return Ok(i);
        }
        let i = self.ends.len() as u32;
        std::io::Write::write_all(&mut self.strs, s.as_bytes())?;
        self.str_len += s.len() as u32;
        self.ends.push(self.str_len);
        self.ids.insert(h, i);
        Ok(i)
    }

    fn opt(&mut self, s: &Option<String>) -> std::io::Result<u32> {
        match s {
            Some(s) => self.id(s),
            None => Ok(NONE),
        }
    }

    fn add(&mut self, c: &Class) -> std::io::Result<()> {
        let simple = c.name.rsplit(['/', '$']).next().unwrap_or(&c.name);
        let sid = self.id(simple)?;
        self.by_simple.push((simple.into(), sid, self.written));
        let mut rec = std::mem::take(&mut self.rec);
        rec.clear();
        let put = |rec: &mut Vec<u8>, v: u32| rec.extend_from_slice(&v.to_le_bytes());
        put(&mut rec, self.id(&c.name)?);
        put(&mut rec, self.opt(&c.super_name)?);
        put(&mut rec, self.opt(&c.signature)?);
        rec.extend_from_slice(&c.access.to_le_bytes());
        rec.extend_from_slice(&(c.interfaces.len() as u16).to_le_bytes());
        for i in &c.interfaces {
            put(&mut rec, self.id(i)?);
        }
        for list in [&c.fields, &c.methods] {
            rec.extend_from_slice(&(list.len() as u16).to_le_bytes());
            for m in list {
                put(&mut rec, self.id(&m.name)?);
                put(&mut rec, self.id(&m.desc)?);
                put(&mut rec, self.opt(&m.signature)?);
                rec.extend_from_slice(&m.access.to_le_bytes());
            }
        }
        std::io::Write::write_all(&mut self.out, &rec)?;
        self.written += rec.len() as u32;
        self.rec = rec;
        Ok(())
    }

    /// 後ろに文字の終わりの位置・文字・単純名の表を書き、頭に位置を書き込む
    fn finish(mut self) -> std::io::Result<()> {
        use std::io::Write;
        self.by_simple.sort_by(|x, y| x.0.cmp(&y.0));
        let ends_at = HEAD as u32 + self.written;
        for e in &self.ends {
            self.out.write_all(&e.to_le_bytes())?;
        }
        let strings_at = ends_at + self.ends.len() as u32 * 4;
        drop(self.strs);
        std::io::copy(&mut File::open(&self.strs_path)?, &mut self.out)?;
        let _ = std::fs::remove_file(&self.strs_path);
        let table_at = strings_at + self.str_len;
        for (_, s, at) in &self.by_simple {
            self.out.write_all(&s.to_le_bytes())?;
            self.out.write_all(&at.to_le_bytes())?;
        }
        let mut head = Vec::with_capacity(HEAD);
        head.extend_from_slice(b"NGJ1");
        for v in [self.ends.len() as u32, self.by_simple.len() as u32, ends_at, strings_at, table_at] {
            head.extend_from_slice(&v.to_le_bytes());
        }
        let mut f = self.out.into_inner().map_err(|e| e.into_error())?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&head)?;
        Ok(())
    }
}

/// jar 1 つのかけらを `out` に書く。読んだクラスの数を返す。`keep` は中の名前で読むものを選ぶ
pub fn write_shard(jar: &Path, keep: impl Fn(&str) -> bool, out: &Path) -> std::io::Result<usize> {
    let mut w = ShardWriter::create(out)?;
    let mut err = None;
    let n = read_jar(jar, keep, |c| {
        if err.is_none() {
            err = w.add(&c).err();
        }
    })?;
    if let Some(e) = err {
        return Err(e);
    }
    w.finish()?;
    Ok(n)
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
        if b.get(0..4)? != b"NGJ1" {
            return None;
        }
        let u32_at = |i: usize| -> Option<usize> { Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?) as usize) };
        let (nstr, nclass, ends, strings, table) = (u32_at(4)?, u32_at(8)?, u32_at(12)?, u32_at(16)?, u32_at(20)?);
        if ends + nstr * 4 != strings || table + nclass * 8 != b.len() {
            return None;
        }
        Some(Shard { b, ends, nstr, strings, classes: HEAD, table, nclass })
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
    // 形を変えたら頭の版も変える（古いかけらは使わない）
    let key = format!("NGJ1|{}|{}|{t}|{tag}", jar.display(), m.len());
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
    /// かけらの元の jar（ct.sym）
    paths: Vec<PathBuf>,
    /// 単純名で並べた (かけら, かけらの中の単純名の表の番号)
    table: Vec<(u16, u32)>,
    /// クラスのあるパッケージ（内部名の `java/util`。並べて重なりなし）
    packages: Vec<Box<str>>,
    /// 作り直したかけらの数
    pub built: usize,
}

impl JarIndex {
    /// かけらを読み（無ければ作って `dir` に書き）、写して束ねる。作る間は jar 1 つ分ずつ
    pub fn load(sources: &[JarSource], dir: &Path) -> JarIndex {
        let _ = std::fs::create_dir_all(dir);
        let mut shards = Vec::new();
        let mut paths = Vec::new();
        let mut built = 0;
        for src in sources {
            let Some(name) = shard_name(src.path, src.tag) else { continue };
            let file = dir.join(name);
            if !file.is_file() {
                // 書きかけを読まないよう、別の名前で書いてから置き換える
                let tmp = file.with_extension(format!("tmp{}", std::process::id()));
                if write_shard(src.path, src.keep, &tmp).is_err() || std::fs::rename(&tmp, &file).is_err() {
                    let _ = std::fs::remove_file(&tmp);
                    continue;
                }
                built += 1;
            }
            if let Some(m) = nagi_core::mmap::Mapped::open(&file).filter(|m| Shard::open(m).is_some()) {
                shards.push(m);
                paths.push(src.path.to_path_buf());
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
        let set: std::collections::BTreeSet<&str> = table.iter().map(|&(i, k)| views[i as usize].name_at(k as usize).rsplit_once('/').map_or("", |x| x.0)).collect();
        let packages: Vec<Box<str>> = set.into_iter().map(Box::from).collect();
        drop(views);
        JarIndex { shards, paths, table, packages, built }
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

    /// 単純名が `prefix` で始まるクラスの内部名（最大 `limit` 個。単純名の順）。補完の候補
    pub fn by_prefix(&self, prefix: &str, limit: usize) -> Vec<&str> {
        if prefix.is_empty() {
            return Vec::new();
        }
        let key = |k: usize| self.shard(self.table[k].0).simple_at(self.table[k].1 as usize);
        let lo = partition(self.table.len(), |k| key(k) < prefix);
        (lo..self.table.len()).take_while(|&k| key(k).starts_with(prefix)).take(limit).map(|k| self.shard(self.table[k].0).name_at(self.table[k].1 as usize)).collect()
    }

    /// すべてのクラスの内部名（かけらを 1 つずつ開いて）
    fn all_names(&self) -> impl Iterator<Item = &str> {
        self.shards.iter().flat_map(|m| {
            let s = Shard::open(m).unwrap();
            (0..s.len()).map(move |k| s.name_at(k))
        })
    }

    /// `pkg`（内部名の `java/util`・`com`）がパッケージか、その上か
    pub fn is_package(&self, pkg: &str) -> bool {
        let i = self.packages.partition_point(|p| &**p < pkg);
        self.packages.get(i).is_some_and(|p| &**p == pkg || p.strip_prefix(pkg).is_some_and(|r| r.starts_with('/')))
    }

    /// `prefix` で始まる一番上のパッケージの名前（`co` → `com`）
    pub fn top_packages(&self, prefix: &str) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        let mut from = prefix.to_string();
        loop {
            let i = self.packages.partition_point(|p| **p < *from);
            let Some(p) = self.packages.get(i).filter(|p| p.starts_with(prefix)) else { break };
            let top = p.split('/').next().unwrap_or(p);
            out.push(top);
            // 次の一番上の名前へ（'/' の次の文字 '0' で、その下のパッケージを飛ばす）
            from = format!("{top}0");
        }
        out
    }

    /// `pkg` の下のパッケージの次の名前（`java` → `util`・`io` …）
    pub fn subpackages(&self, pkg: &str) -> Vec<&str> {
        let head = format!("{pkg}/");
        let i = self.packages.partition_point(|p| **p < *head);
        let mut out: Vec<&str> = Vec::new();
        for p in self.packages[i..].iter().take_while(|p| p.starts_with(&head)) {
            let next = p[head.len()..].split('/').next().unwrap_or("");
            if out.last() != Some(&next) {
                out.push(next);
            }
        }
        out
    }

    /// `pkg` の中の（入れ子でない）クラスの内部名
    pub fn classes_in(&self, pkg: &str) -> Vec<&str> {
        if !self.packages.binary_search_by(|p| (**p).cmp(pkg)).is_ok() {
            return Vec::new();
        }
        self.all_names()
            .filter(|n| n.len() > pkg.len() && n.starts_with(pkg) && n.as_bytes()[pkg.len()] == b'/' && !n[pkg.len() + 1..].contains(['/', '$']))
            .collect()
    }

    /// `outer`（内部名）のすぐ中の入れ子のクラスの内部名（`java/util/Map` → `java/util/Map$Entry`）
    pub fn nested(&self, outer: &str) -> Vec<&str> {
        let head = format!("{outer}$");
        self.all_names().filter(|n| n.strip_prefix(&head).is_some_and(|r| !r.contains('$') && !r.starts_with(|c: char| c.is_ascii_digit()))).collect()
    }

    /// クラスのある jar（JDK のクラスは ct.sym）
    pub fn jar_of(&self, internal: &str) -> Option<&Path> {
        let simple = internal.rsplit(['/', '$']).next().unwrap_or(internal);
        let key = |k: usize| self.shard(self.table[k].0).simple_at(self.table[k].1 as usize);
        let lo = partition(self.table.len(), |k| key(k) < simple);
        (lo..self.table.len()).take_while(|&k| key(k) == simple).find(|&k| self.shard(self.table[k].0).name_at(self.table[k].1 as usize) == internal).map(|k| self.paths[self.table[k].0 as usize].as_path())
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

/// ct.sym の、型のある一番新しい版の文字（上のフォルダー名の文字。`7`〜`9`・`A`〜）。JDK N の ct.sym は N-1 までの型を持つ
pub fn ct_sym_latest(ct: &Path) -> Option<char> {
    let mut f = File::open(ct).ok()?;
    // 版のフォルダーは数字と大文字だけの名前（META-INF などは除く）
    let release = |d: &str| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit() || c.is_ascii_uppercase());
    zip_entries(&mut f).ok()?.iter().filter(|e| e.name.ends_with(".sig")).filter_map(|e| e.name.split_once('/').map(|x| x.0)).filter(|d| release(d)).flat_map(|d| d.chars()).max()
}

/// プロジェクトの JDK（設定の JDK・使える JAVA_HOME・見つけた JDK）
fn jdk_home(cfg: &nagi_workers::config::Config) -> Option<PathBuf> {
    nagi_workers::jobs::java_home_fix(cfg).or_else(|| cfg.var("JAVA_HOME").map(PathBuf::from)).filter(|j| j.join("lib").join("ct.sym").is_file())
}

/// Maven の出力（`;` 区切り。モジュールごとに書き足すと間の `;` が無い）を jar の並びにする
fn split_classpath(text: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for part in text.split([';', '\n', '\r']) {
        // `…\a.jarC:\…` を 2 つに
        let mut rest = part.trim();
        while !rest.is_empty() {
            let cut = rest.match_indices(".jar").map(|(i, _)| i + 4).find(|&i| rest[i..].len() >= 3 && rest.as_bytes()[i].is_ascii_alphabetic() && &rest[i + 1..i + 3] == ":\\").unwrap_or(rest.len());
            let p = PathBuf::from(&rest[..cut]);
            if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("jar")) && !out.contains(&p) {
                out.push(p);
            }
            rest = &rest[cut..];
        }
    }
    out
}

/// プロジェクトの pom.xml（target・node_modules・隠しフォルダーは見ない。4 段まで）の (パス, 更新時刻) を並べた文字
fn pom_stamp(root: &Path) -> String {
    let mut v = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((d, depth)) = stack.pop() {
        let pom = d.join("pom.xml");
        if let Ok(m) = std::fs::metadata(&pom) {
            let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |t| t.as_secs());
            v.push(format!("{}|{t}", pom.display()));
        }
        if depth < 4 {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if e.file_type().is_ok_and(|t| t.is_dir()) && !n.starts_with('.') && !matches!(n.as_str(), "target" | "node_modules" | "src") {
                    stack.push((e.path(), depth + 1));
                }
            }
        }
    }
    v.sort();
    v.join("\n")
}

/// プロファイルの jdk の条件に使う版（プロジェクトの JDK。わからなければ 17）
fn java_major(cfg: &nagi_workers::config::Config) -> u32 {
    nagi_workers::jobs::java_home_fix(cfg).or_else(|| cfg.var("JAVA_HOME").map(PathBuf::from)).and_then(|j| nagi_workers::jdk::read(&j)).and_then(|j| j.major()).unwrap_or(17)
}

/// Maven のプロジェクトの依存の jar（コンパイルとテストに使うもの）。まず pom.xml と手元のリポジトリから求め
/// （mvn も JDK も起動しない。モジュールどうしの分は除く）、手元に無いものがあれば mvn に任せる（ダウンロードもする）
pub fn maven_classpath(root: &Path) -> Result<Vec<PathBuf>, String> {
    let java = java_major(&nagi_workers::config::for_path(root));
    match nagi_workers::maven_deps::resolve(root, java) {
        Ok(r) if r.missing.is_empty() => Ok(r.jars),
        Ok(r) => {
            nagi_core::log::info(&format!("手元のリポジトリに無い依存が {} 個（{} など）。mvn で求めます", r.missing.len(), r.missing[0]));
            mvn_classpath(root).or_else(|e| if r.jars.is_empty() { Err(e) } else { Ok(r.jars) })
        }
        Err(e) => mvn_classpath(root).map_err(|m| format!("{e}。{m}")),
    }
}

/// mvn で依存を求める。pom.xml が変わっていなければ前に求めたもの。
/// mvn はヒープを 64 MB に絞って動かす（そのままだと一時的に 700 MB を超えた。絞ると約 130 MB・7 秒）
fn mvn_classpath(root: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = jars_dir().ok_or("LOCALAPPDATA がありません")?.with_file_name("classpath");
    let _ = std::fs::create_dir_all(&dir);
    let key = nagi_core::hash::sha256_hex(root.display().to_string().to_lowercase().as_bytes()).ok_or("hash")?;
    let file = dir.join(format!("{}.txt", &key[..24]));
    let stamp = pom_stamp(root);
    let stamp_hash = nagi_core::hash::sha256_hex(stamp.as_bytes()).unwrap_or_default();
    if let Ok(text) = std::fs::read_to_string(&file) {
        if let Some((head, body)) = text.split_once('\n') {
            if head == stamp_hash {
                return Ok(split_classpath(body));
            }
        }
    }
    let mut ex = nagi_workers::exec::Exec::in_dir(root).env("MAVEN_OPTS", "-Xmx64m -XX:+UseSerialGC -XX:TieredStopAtLevel=1 -Xss512k -XX:ReservedCodeCacheSize=16m");
    ex.fix_jdk();
    let mvn = nagi_workers::maven::bin(ex.config(), ex.config().var("PATH")).map(|b| b.join("mvn.cmd")).ok_or("Maven が見つかりません")?;
    // 出力の「Dependencies classpath:」の次の行がモジュールごとのクラスパス（-Dmdep.outputFile と appendOutput では
    // モジュールの分が欠けた）。まず手元の .m2 だけで（-o）、足りなければダウンロードも許す
    let mut why = String::new();
    let mut text = String::new();
    for offline in ["-o ", ""] {
        let line = format!("\"{}\" -B {offline}dependency:build-classpath", mvn.display());
        // cmd /C は行が " で始まると最初と最後の " を外すので、行全体をもう一度囲む
        match ex.shell(&format!("\"{line}\"")).output() {
            Ok(o) => {
                let log = nagi_workers::jobs::decode_line(&o.stdout);
                let mut lines = log.lines();
                while let Some(l) = lines.next() {
                    if l.contains("Dependencies classpath:") {
                        text.push_str(lines.next().unwrap_or(""));
                        text.push(';');
                    }
                }
                if o.status.success() {
                    break;
                }
                why = log.lines().map(str::trim).find(|l| l.contains("ERROR") || l.contains("JAVA_HOME")).unwrap_or("").to_string();
                text.clear();
            }
            Err(e) => why = e.to_string(),
        }
    }
    if text.is_empty() {
        return Err(format!("Maven で依存を求められません（mvn dependency:build-classpath）: {why}"));
    }
    let jars = split_classpath(&text);
    let body: Vec<String> = jars.iter().map(|p| p.display().to_string()).collect();
    let _ = std::fs::write(&file, format!("{stamp_hash}\n{}", body.join("\n")));
    Ok(jars)
}

/// Maven のプロジェクトの型の索引（依存の jar と JDK）を裏で用意する。終わったら 1 度だけ送る（Err は訳）
pub fn load_maven(root: PathBuf, waker: nagi_workers::Waker) -> nagi_core::task::Task<Result<JarIndex, String>> {
    nagi_core::task::spawn("jar-index", waker, move |ctx| {
        ctx.send(load_maven_now(&root));
    })
}

/// `load_maven` をこのスレッドで
pub fn load_maven_now(root: &Path) -> Result<JarIndex, String> {
    let jars = maven_classpath(root)?;
    let cfg = nagi_workers::config::for_path(root);
    let ct = jdk_home(&cfg).map(|j| j.join("lib").join("ct.sym"));
    let rel = ct.as_deref().and_then(ct_sym_latest);
    let all = |_: &str| true;
    let jdk = |n: &str| rel.is_some_and(|r| n.split('/').next().is_some_and(|d| d.contains(r)));
    let tag = rel.map(String::from).unwrap_or_default();
    let mut sources: Vec<JarSource> = jars.iter().map(|p| JarSource { path: p, keep: &all, tag: "" }).collect();
    if let Some(ct) = &ct {
        sources.push(JarSource { path: ct, keep: &jdk, tag: &tag });
    }
    let dir = jars_dir().ok_or("LOCALAPPDATA がありません")?;
    Ok(JarIndex::load(&sources, &dir))
}

/// 生態系 JVM（Maven のプロジェクトの依存の jar と JDK。Java と Kotlin が使う）
pub struct Jvm;

pub static ECO: Jvm = Jvm;

impl super::Ecosystem for Jvm {
    fn name(&self) -> &'static str {
        super::JVM
    }

    fn root(&self, root: &Path) -> Option<PathBuf> {
        nagi_workers::maven::find_root(root).filter(|m| m.join("pom.xml").is_file())
    }

    fn load(&self, root: &Path) -> Result<super::Loaded, String> {
        let ix = load_maven_now(root)?;
        let log = format!("ライブラリの型の索引: {} クラス（作り直したかけら {}）", ix.classes(), ix.built);
        Ok(super::Loaded { index: std::sync::Arc::new(ix), log })
    }

    fn rebuild_on(&self, path: &Path) -> bool {
        path.file_name().is_some_and(|n| n.eq_ignore_ascii_case("pom.xml")) || path.ends_with(".mvn/maven.config")
    }

    fn index_build_file(&self) -> bool {
        false
    }

    fn hidden_build_files(&self) -> &'static [&'static str] {
        &[".mvn/maven.config"]
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
        std::env::var_os("JAVA_HOME").map(Into::into).or_else(|| nagi_workers::jdk::installed().into_iter().next().map(|j| j.path))
    }

    fn private_bytes() -> usize {
        nagi_workers::sys::private_peak().0
    }

    /// 実際の Maven のプロジェクトで依存を求める（NAGI_MAVEN_ROOT に pom.xml のあるフォルダ。
    /// cargo test -p nagi-workers maven_classpath_real -- --ignored --nocapture）
    #[test]
    #[ignore]
    fn maven_classpath_real() {
        let Some(root) = std::env::var_os("NAGI_MAVEN_ROOT") else { return };
        let t = std::time::Instant::now();
        let r = maven_classpath(Path::new(&root));
        println!("{:?} jars, {} ms", r.map(|v| v.len()), t.elapsed().as_millis());
    }

    #[test]
    fn classpath_split() {
        let t = "C:\\m\\a.jar;C:\\m\\b.jarC:\\m\\c.jar;C:\\m\\a.jar\r\nC:\\x\\classes;D:\\m\\d.jar";
        let v: Vec<String> = split_classpath(t).iter().map(|p| p.display().to_string()).collect();
        assert_eq!(v, vec!["C:\\m\\a.jar", "C:\\m\\b.jar", "C:\\m\\c.jar", "D:\\m\\d.jar"]);
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
        let file = std::env::temp_dir().join(format!("nagi-jvm-list-{}.ngj", std::process::id()));
        let n = write_shard(&ct, keep, &file).unwrap();
        assert!(n > 100, "{n}");
        let bytes = std::fs::read(&file).unwrap();
        let _ = std::fs::remove_file(&file);
        let s = Shard::open(&bytes).unwrap();
        assert_eq!(s.by_simple_name("List"), vec!["java/util/List"]);
        assert_eq!(ct_sym_latest(&ct), Some('G'), "JDK 17 の ct.sym は 16（G）まで");
        let list = s.class("java/util/List").unwrap();
        assert_eq!(list.interfaces, vec!["java/util/Collection"]);
        assert_eq!(list.signature.as_deref(), Some("<E:Ljava/lang/Object;>Ljava/lang/Object;Ljava/util/Collection<TE;>;"));
        let get = list.methods.iter().find(|m| m.name == "get").unwrap();
        assert_eq!((get.desc.as_str(), get.signature.as_deref()), ("(I)Ljava/lang/Object;", Some("(I)TE;")));
        assert_eq!(s.by_simple_name("Entry"), vec!["java/util/Map$Entry"]);
        // 写して束ねた索引でも引ける（単純名・前方一致）
        let dir = std::env::temp_dir().join(format!("nagi-jvm-{}", std::process::id()));
        let ix = JarIndex::load(&[JarSource { path: &ct, keep: &keep, tag: "G-util" }], &dir);
        assert_eq!(ix.built, 1);
        assert_eq!(ix.by_simple_name("ArrayList"), vec!["java/util/ArrayList"]);
        assert!(ix.by_prefix("ArrayL", 10) == vec!["java/util/ArrayList"]);
        assert!(ix.class("java/util/List").is_some_and(|c| c.methods.iter().any(|m| m.name == "get")));
        // 2 回目は作らずに写すだけ
        assert_eq!(JarIndex::load(&[JarSource { path: &ct, keep: &keep, tag: "G-util" }], &dir).built, 0);
        let _ = std::fs::remove_dir_all(&dir);
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
        let peak = nagi_workers::sys::private_peak().1;
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
            "{} jars + JDK → {} classes ({} shards built), on disk {:.1} MB, load {} ms, peak {:.1} MB, private {:.1} → {:.1} MB after load → {:.1} MB after lookups, by_simple_name {:?} ({} hits), class() {:?}",
            jars.len(), ix.classes(), ix.built, disk as f64 / 1_048_576.0, load.as_millis(), mb(peak), mb(m0), mb(m1), mb(m2), per_simple, hits / 1000, per_class
        );
    }
}
