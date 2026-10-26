//! 画面の部品の寸法を 1 か所で決める（アイコンの大きさ・アイコンの後ろの幅・角の丸み・影・線の太さ・小窓の幅）。
//!
//! どれも「部品の文字の大きさ（em。設定の『画面の部品の文字の大きさ』）」と行の高さ・セルの幅から求める。
//! 文字を大きくすればアイコンも余白も同じ割合で大きくなる。数字を直に書かず、ここの関数を使う。
//! 描くたびに view.rs の build が `set` で入れ直す（試験では既定の値のまま）。

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// 部品の文字の大きさ（px）・セルの幅・セルの高さ（f32 のビット）
static EM: AtomicU32 = AtomicU32::new(0x4180_0000); // 16.0
static CELL_W: AtomicU32 = AtomicU32::new(0x4110_0000); // 9.0
static CELL_H: AtomicU32 = AtomicU32::new(0x41a0_0000); // 20.0
/// 部品の文字の、等幅の文字に対する幅の倍率（×100。部品の文字を本文より大きくしたとき）
static UI_SCALE: AtomicUsize = AtomicUsize::new(100);

fn load(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

/// 寸法の元を入れる（描くたび）
pub fn set(em: f32, cell_w: f32, cell_h: f32, ui_ratio: f32) {
    EM.store(em.max(6.0).to_bits(), Ordering::Relaxed);
    CELL_W.store(cell_w.max(1.0).to_bits(), Ordering::Relaxed);
    CELL_H.store(cell_h.max(1.0).to_bits(), Ordering::Relaxed);
    UI_SCALE.store((ui_ratio.max(1.0) * 100.0).round() as usize, Ordering::Relaxed);
}

/// 部品の文字の大きさ（px）
pub fn em() -> f32 {
    load(&EM)
}

/// アイコンの大きさの種類（IntelliJ の 16・20・32 に当たる）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    /// 一覧・タブ・入力欄・吹き出し（行に収まる）
    Small,
    /// 左右のアイコンの列・上の帯のボタン
    Large,
    /// ダイアログの問いの印
    Dialog,
}

/// アイコンの大きさ（px）
pub fn icon(size: Icon) -> f32 {
    let (em, h) = (em(), load(&CELL_H));
    match size {
        Icon::Small => (em * 1.15).round().min(h - 2.0).max(8.0),
        Icon::Large => (em * 1.45).round().min(h).max(10.0),
        Icon::Dialog => (em * 2.3).round().max(16.0),
    }
}

/// アイコンとその後ろの隙間が使う列の数（アイコンの後ろの文字はここから）
pub fn icon_cols(size: Icon) -> usize {
    ((icon(size) + em() * 0.35) / load(&CELL_W)).ceil() as usize
}

/// 行 `y`（px）の中で、`px` のものを縦の真ん中に置く y
pub fn centered(y: f32, px: f32) -> f32 {
    y + ((load(&CELL_H) - px) / 2.0).round()
}

/// 角の丸みの種類
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Radius {
    /// 小窓・ダイアログ・吹き出し
    Popup,
    /// ボタン・選んでいる行・アイコンの地
    Control,
    /// 頭文字の四角などの小さなもの
    Small,
}

/// 角の丸み（px）
pub fn radius(r: Radius) -> f32 {
    let em = em();
    match r {
        Radius::Popup => (em * 0.55).round(),
        Radius::Control => (em * 0.3).round().max(2.0),
        Radius::Small => (em * 0.22).round().max(2.0),
    }
}

/// 小窓の影のぼかし（px）
pub fn shadow() -> f32 {
    (em() * 1.0).round()
}

/// 部品の中の小さな余白（ボタンの上下・選んでいる行の左右など。px）
pub fn pad() -> f32 {
    (em() * 0.15).round().max(1.0)
}

/// 強調の線（選んでいるタブの下線など。px）
pub fn accent_line() -> f32 {
    (em() * 0.18).round().max(2.0)
}

/// 部品の文字で書いた `s` の幅（列）。部品の文字を本文より大きくしていれば、その分広く
pub fn ui_width(s: &str) -> usize {
    cols(nagi_core::display_col(s, s.len(), false))
}

/// 部品の文字で `n` 列ぶん（小窓の幅・折り返しの幅など）
pub fn cols(n: usize) -> usize {
    (n * UI_SCALE.load(Ordering::Relaxed)).div_ceil(100)
}

/// 部品の文字で `w` 列の中に入る、等幅の文字の列の数（折り返しの見積もり。部品の文字は等幅の 9 割ほど）
pub fn fit(w: usize) -> usize {
    w * 115 * 100 / (100 * UI_SCALE.load(Ordering::Relaxed))
}

/// 左右のアイコンの列の幅（列。大きいアイコンと左右の余白）
pub fn stripe_w() -> usize {
    ((icon(Icon::Large) + em() * 1.0) / load(&CELL_W)).ceil().clamp(3.0, 10.0) as usize
}
