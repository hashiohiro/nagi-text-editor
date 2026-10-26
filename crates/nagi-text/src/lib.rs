//! フォント解決（DirectWrite）とラスタライズ（swash）、セル幅計算。ADR 0004。
//!
//! - 設定順のフォントチェーンを名前で遅延解決する（全列挙しない）。
//! - 文字ごとにチェーンの cmap で face を選び、どれにも無ければ `MapCharacters` で OS に聞く。
//! - 主フォントからセル寸法を決め、副フォントは縮尺とベースラインをセルに合わせる。

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// フォントのファイルを写したもの（読み込むと日本語フォントの 5 MB が窓ごとの private になる）
pub use nagi_core::mmap::Mapped as FontBytes;

use dwrote::{
    FontCollection, FontFallback, FontStretch, FontStyle, FontWeight, TextAnalysisSource, TextAnalysisSourceMethods,
};
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::Format;
use swash::FontRef;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub const DEFAULT_FONTS: &[&str] = &["Cascadia Mono", "BIZ UDGothic", "Yu Gothic", "Symbols Nerd Font Mono", "Segoe UI Emoji"];

/// フォントファイルの中身（起動時は別スレッドで読む）
#[derive(Clone)]
pub struct FontData {
    pub name: String,
    /// ファイルの中身（メモリに写したもの。窓どうしで共有する）
    pub data: Arc<FontBytes>,
    pub index: u32,
}

/// Windows に入っているフォントのファミリー名と、等幅か（設定の画面のフォントの一覧。等幅を先に、名前の順）
pub fn font_families() -> Vec<(String, bool)> {
    let collection = FontCollection::system();
    let mut out: Vec<(String, bool)> = collection
        .families_iter()
        .filter_map(|f| {
            let name = f.family_name().ok()?;
            let mono = f.first_matching_font(FontWeight::Regular, FontStretch::Normal, FontStyle::Normal).ok().and_then(|font| font.is_monospace()).unwrap_or(false);
            Some((name, mono))
        })
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// 名前でフォントを解決し、ファイルを読む。見つからなければ None。
pub fn load_by_name(name: &str) -> Option<FontData> {
    let collection = FontCollection::system();
    let family = collection.font_family_by_name(name).ok()??;
    let font = family.first_matching_font(FontWeight::Regular, FontStretch::Normal, FontStyle::Normal).ok()?;
    let (data, index, _) = load_font(&font)?;
    Some(FontData { name: name.into(), data, index })
}

fn load_font(font: &dwrote::Font) -> Option<(Arc<FontBytes>, u32, PathBuf)> {
    let face = font.create_font_face();
    let file = face.files().ok()?.into_iter().next()?;
    let path = file.font_file_path().ok()?;
    let data = FontBytes::open(&path)?;
    Some((data, face.get_index(), path))
}

#[derive(Clone, Copy, Debug)]
pub struct CellSize {
    pub w: f32,
    pub h: f32,
}

struct Face {
    font: FontData,
    /// この face の描画倍率（主フォントは 1.0）
    scale: f32,
    /// セル上端からベースラインまで（px）
    baseline: f32,
    color: bool,
}

impl Face {
    fn font_ref(&self) -> FontRef<'_> {
        FontRef::from_index(&self.font.data[..], self.font.index as usize).unwrap()
    }
}

/// ラスタライズ済みグリフ。`left`/`top` は割り当てセル左上からのオフセット（px）。
pub struct GlyphImage {
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    /// true なら RGBA8、false なら A8
    pub color: bool,
    pub data: Vec<u8>,
}

/// 描画のたびに 1 文字ごとに引く表のためのハッシュ（rustc の FxHash と同じ方式）。
/// 既定の SipHash は HashDoS に強い代わりに遅い。キーは自分で作る値だけなので、こちらで足りる。
#[derive(Default, Clone, Copy)]
pub struct FxHasher(u64);

impl std::hash::Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(b as u64);
        }
    }
    fn write_u8(&mut self, i: u8) {
        self.write_u64(i as u64);
    }
    fn write_u16(&mut self, i: u16) {
        self.write_u64(i as u64);
    }
    fn write_u32(&mut self, i: u32) {
        self.write_u64(i as u64);
    }
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    fn write_usize(&mut self, i: usize) {
        self.write_u64(i as u64);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

pub type FastMap<K, V> = HashMap<K, V, std::hash::BuildHasherDefault<FxHasher>>;

/// グリフの識別子（face 番号＋グリフ ID＋セル幅）。アトラスのキーに使う。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GlyphKey {
    pub face: u16,
    pub glyph: u16,
    pub width: u8,
}

pub struct Fonts {
    px: f32,
    /// 行の高さの倍率（1.0 で文字の高さそのまま）
    line: f32,
    cell: CellSize,
    faces: Vec<Face>,
    /// MapCharacters で追加した face（ファミリー名 → faces の添字）
    extra: HashMap<String, usize>,
    /// char → (face, グリフ ID)（None は豆腐）
    pick_cache: FastMap<char, Option<(u16, u16)>>,
    ctx: ScaleContext,
    pub ambiguous_wide: bool,
    /// 画面の部品（ツールバー・木・ダイアログなど）の文字（等幅でないフォント。IntelliJ の UI の文字と同じ）
    pub ui: Option<UiFont>,
    /// 部品の文字の大きさの、本文の文字に対する比（設定の「画面の部品の文字の大きさ」）
    ui_ratio: f32,
}

impl Fonts {
    /// `chain[0]` を主フォントとして、`px` ピクセルでセル寸法を決める。
    pub fn new(chain: Vec<FontData>, px: f32) -> Self {
        assert!(!chain.is_empty(), "主フォントが解決できない");
        let mut faces: Vec<Face> = chain
            .into_iter()
            .map(|font| {
                let color = is_color(&font);
                Face { font, scale: 1.0, baseline: 0.0, color }
            })
            .collect();
        let cell = cell_metrics(&faces[0], px, 1.0);
        for f in faces.iter_mut() {
            fit_face(f, px, cell);
        }
        Self { px, line: 1.0, cell, faces, extra: HashMap::new(), pick_cache: FastMap::default(), ctx: ScaleContext::new(), ambiguous_wide: false, ui: None, ui_ratio: 1.0 }
    }

    pub fn cell(&self) -> CellSize {
        self.cell
    }

    /// 行の高さの倍率（設定の「行の高さ」）。セルの高さを変え、文字は上下の真ん中に置く
    pub fn set_line_height(&mut self, line: f32) {
        self.line = line.clamp(0.8, 2.0);
        self.set_px(self.px);
    }

    /// 文字の大きさを変える（表示倍率が変わったとき）。描いたグリフは作り直しになる
    pub fn set_px(&mut self, px: f32) {
        self.px = px;
        self.cell = cell_metrics(&self.faces[0], px, self.line);
        // 部品の文字のほうが大きければ、行をその高さに広げる（本文は行の真ん中に）
        if let Some(ui) = &self.ui {
            self.cell.h = self.cell.h.max(ui.line_height(px * self.ui_ratio));
        }
        for f in self.faces.iter_mut() {
            fit_face(f, px, self.cell);
        }
        let ui_px = px * self.ui_ratio;
        if let Some(ui) = &mut self.ui {
            ui.resize(ui_px, self.cell.h);
        }
    }

    /// 部品の文字の大きさの、本文の文字に対する比
    pub fn ui_ratio(&self) -> f32 {
        self.ui_ratio
    }

    /// 部品の文字の大きさを本文の文字の `ratio` 倍にする（行の高さも合わせ直す）
    pub fn set_ui_ratio(&mut self, ratio: f32) {
        self.ui_ratio = ratio.clamp(0.5, 3.0);
        self.set_px(self.px);
    }

    /// 画面の部品の文字のフォントを使う（`chain` は前から探す。見つからない字はセルの文字で描く）
    pub fn enable_ui(&mut self, chain: Vec<FontData>) {
        self.ui = (!chain.is_empty()).then(|| UiFont::new(chain, self.px * self.ui_ratio, self.cell.h));
        self.set_px(self.px);
    }

    /// 書記素のセル幅（1 か 2）
    pub fn width(&self, grapheme: &str) -> usize {
        if self.ambiguous_wide || nagi_core::ambiguous_wide() { grapheme.width_cjk() } else { grapheme.width() }.clamp(1, 2)
    }

    /// 書記素を描くグリフ。空白や豆腐は None。シェーピングはしないので先頭のスカラーだけを描く。
    pub fn glyph_for(&mut self, grapheme: &str, width: usize) -> Option<GlyphKey> {
        let ch = grapheme.chars().next()?;
        if ch == ' ' || ch == '\u{3000}' || ch == '\t' {
            return None;
        }
        let (face, glyph) = match self.pick_cache.get(&ch) {
            Some(f) => (*f)?,
            None => {
                let f = self.pick(ch, grapheme).map(|face| (face, self.faces[face as usize].font_ref().charmap().map(ch)));
                self.pick_cache.insert(ch, f);
                f?
            }
        };
        Some(GlyphKey { face, glyph, width: width as u8 })
    }

    fn pick(&mut self, ch: char, grapheme: &str) -> Option<u16> {
        if let Some(i) = self.faces.iter().position(|f| f.font_ref().charmap().map(ch) != 0) {
            return Some(i as u16);
        }
        // 最後の手段: OS のフォールバック（初回は数十 ms かかることがある。S2）
        let (name, data, index) = map_characters(grapheme, &self.faces[0].font.name)?;
        if let Some(&i) = self.extra.get(&name) {
            return Some(i as u16);
        }
        let font = FontData { name: name.clone(), data, index };
        let color = is_color(&font);
        let mut f = Face { font, scale: 1.0, baseline: 0.0, color };
        fit_face(&mut f, self.px, self.cell);
        if f.font_ref().charmap().map(ch) == 0 {
            return None;
        }
        self.faces.push(f);
        self.extra.insert(name, self.faces.len() - 1);
        Some((self.faces.len() - 1) as u16)
    }

    pub fn rasterize(&mut self, key: GlyphKey) -> Option<GlyphImage> {
        raster(&self.faces, self.px, self.cell, &mut self.ctx, key)
    }

    /// 複数のグリフを CPU コア数ぶんのスレッドで並列にラスタライズする（起動時の最初の画面用）
    pub fn rasterize_many(&mut self, keys: &[GlyphKey]) -> Vec<(GlyphKey, Option<GlyphImage>)> {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
        if keys.len() < 64 || threads < 2 {
            return keys.iter().map(|&k| (k, self.rasterize(k))).collect();
        }
        let (faces, px, cell) = (&self.faces, self.px, self.cell);
        let chunk = keys.len().div_ceil(threads);
        std::thread::scope(|sc| {
            let handles: Vec<_> = keys
                .chunks(chunk)
                .map(|part| {
                    sc.spawn(move || {
                        let mut ctx = ScaleContext::new();
                        part.iter().map(|&k| (k, raster(faces, px, cell, &mut ctx, k))).collect::<Vec<_>>()
                    })
                })
                .collect();
            handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
        })
    }
}

fn raster(faces: &[Face], px: f32, cell: CellSize, ctx: &mut ScaleContext, key: GlyphKey) -> Option<GlyphImage> {
    let face = &faces[key.face as usize];
    let font = face.font_ref();
    let size = px * face.scale;
    let mut scaler = ctx.builder(font).size(size).hint(true).build();
    let sources: &[Source] = if face.color {
        &[Source::ColorOutline(0), Source::ColorBitmap(StrikeWith::BestFit), Source::Outline]
    } else {
        &[Source::Outline]
    };
    let img = Render::new(sources).format(Format::Alpha).render(&mut scaler, key.glyph)?;
    let adv = font.glyph_metrics(&[]).scale(size).advance_width(key.glyph);
    let slot_w = key.width as f32 * cell.w;
    // 主フォントの1セル文字は左詰め、それ以外（全角・副フォント）は割当幅の中央へ
    let x_off = if key.face == 0 && key.width == 1 { 0.0 } else { ((slot_w - adv) / 2.0).max(0.0) };
    let (color, data) = match img.content {
        Content::Mask => (false, img.data),
        Content::Color => (true, img.data),
        Content::SubpixelMask => (false, img.data.chunks(4).map(|p| p[1]).collect()),
    };
    Some(GlyphImage {
        width: img.placement.width,
        height: img.placement.height,
        left: (x_off + img.placement.left as f32).round() as i32,
        top: (face.baseline - img.placement.top as f32).round() as i32,
        color,
        data,
    })
}

/// 行を書記素に区切り、(書記素, 開始 byte, セル幅) を返す
pub fn graphemes<'a>(fonts: &'a Fonts, line: &'a str) -> impl Iterator<Item = (&'a str, usize, usize)> + 'a {
    line.grapheme_indices(true).map(move |(i, g)| (g, i, if g == "\t" { nagi_core::tab_width() } else { fonts.width(g) }))
}

fn is_color(f: &FontData) -> bool {
    let r = FontRef::from_index(&f.data[..], f.index as usize).unwrap();
    [b"COLR", b"CBDT", b"sbix"].iter().any(|tag| r.table(u32::from_be_bytes(**tag)).is_some())
}

fn cell_metrics(primary: &Face, px: f32, line: f32) -> CellSize {
    let font = primary.font_ref();
    let m = font.metrics(&[]).scale(px);
    let gid = font.charmap().map('0');
    let adv = font.glyph_metrics(&[]).scale(px).advance_width(gid);
    CellSize { w: adv.round().max(1.0), h: ((m.ascent + m.descent + m.leading) * line).round().max(1.0) }
}

/// 副フォントを主フォントのセル高さに収める。主フォントは等倍・ascent 基準。
fn fit_face(f: &mut Face, px: f32, cell: CellSize) {
    let m = f.font_ref().metrics(&[]).scale(px);
    let content = m.ascent + m.descent;
    f.scale = if content > cell.h { cell.h / content } else { 1.0 };
    f.baseline = (cell.h - content * f.scale) / 2.0 + m.ascent * f.scale;
}

struct LocaleJa;

impl TextAnalysisSourceMethods for LocaleJa {
    fn get_locale_name(&self, _pos: u32) -> (Cow<'_, str>, u32) {
        ("ja-JP".into(), u32::MAX)
    }
    fn get_paragraph_reading_direction(&self) -> u32 {
        0 // DWRITE_READING_DIRECTION_LEFT_TO_RIGHT
    }
}

fn map_characters(text: &str, base_family: &str) -> Option<(String, Arc<FontBytes>, u32)> {
    let collection = FontCollection::system();
    let fallback = FontFallback::get_system_fallback()?;
    let wide: Vec<u16> = text.encode_utf16().collect();
    let len = wide.len() as u32;
    let source = TextAnalysisSource::from_text(Box::new(LocaleJa), Cow::Owned(wide));
    let r = fallback.map_characters(
        &source,
        0,
        len,
        &collection,
        Some(base_family),
        FontWeight::Regular,
        FontStyle::Normal,
        FontStretch::Normal,
    );
    let font = r.mapped_font?;
    let (data, index, _) = load_font(&font)?;
    Some((font.family_name(), data, index))
}

/// 画面の部品の文字のフォントの名前（前から探す）。Windows の UI と同じ
pub const UI_FONTS: &[&str] = &["Segoe UI", "Yu Gothic UI", "Meiryo UI", "Segoe UI Symbol"];

/// 等幅でない文字（画面の部品用）。字ごとに幅を持ち、行の高さの真ん中に置く
pub struct UiFont {
    faces: Vec<FontData>,
    px: f32,
    /// 行の上端からベースラインまで（px）
    baseline: f32,
    /// char → (face, グリフ ID, 送り幅 px)。どの face にも無ければ None
    cache: std::cell::RefCell<FastMap<char, Option<(u16, u16, f32)>>>,
    ctx: ScaleContext,
}

impl UiFont {
    fn new(faces: Vec<FontData>, px: f32, cell_h: f32) -> Self {
        let mut u = UiFont { faces, px: 0.0, baseline: 0.0, cache: Default::default(), ctx: ScaleContext::new() };
        u.resize(px, cell_h);
        u
    }

    /// `px` の大きさで描くときの 1 行の高さ
    fn line_height(&self, px: f32) -> f32 {
        let m = self.font_ref(0).metrics(&[]).scale(px.round());
        (m.ascent + m.descent + m.leading).round()
    }

    fn font_ref(&self, i: usize) -> FontRef<'_> {
        FontRef::from_index(&self.faces[i].data[..], self.faces[i].index as usize).unwrap()
    }

    fn resize(&mut self, px: f32, cell_h: f32) {
        self.px = px.round();
        let m = self.font_ref(0).metrics(&[]).scale(self.px);
        self.baseline = ((cell_h - (m.ascent + m.descent)) / 2.0 + m.ascent).round();
        self.cache.borrow_mut().clear();
    }

    /// 文字の大きさ（px。アトラスの鍵に使う）
    pub fn px(&self) -> f32 {
        self.px
    }

    /// 字の (face, グリフ, 送り幅)。どの face にも無ければ None（セルの文字で描く）
    pub fn glyph(&self, ch: char) -> Option<(u16, u16, f32)> {
        if let Some(g) = self.cache.borrow().get(&ch) {
            return *g;
        }
        let px = self.px;
        let found = (0..self.faces.len()).find_map(|i| {
            let f = self.font_ref(i);
            let gid = f.charmap().map(ch);
            (gid != 0).then(|| (i as u16, gid, f.glyph_metrics(&[]).scale(px).advance_width(gid)))
        });
        self.cache.borrow_mut().insert(ch, found);
        found
    }

    /// 文字列の幅（px。無い字は `fallback` の幅で数える）
    pub fn width(&self, s: &str, fallback: f32) -> f32 {
        s.chars().map(|c| self.glyph(c).map_or(fallback, |g| g.2)).sum()
    }

    /// グリフを描く（`left`・`top` は字の送りの始まりと行の上端からの位置）
    pub fn rasterize(&mut self, face: u16, glyph: u16) -> Option<GlyphImage> {
        let font = FontRef::from_index(&self.faces[face as usize].data[..], self.faces[face as usize].index as usize)?;
        let mut scaler = self.ctx.builder(font).size(self.px).hint(true).build();
        let img = Render::new(&[Source::Outline]).format(Format::Alpha).render(&mut scaler, glyph)?;
        Some(GlyphImage { width: img.placement.width, height: img.placement.height, left: img.placement.left, top: (self.baseline - img.placement.top as f32).round() as i32, color: false, data: img.data })
    }
}
