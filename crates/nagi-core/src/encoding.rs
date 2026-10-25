//! ファイルの文字コード（読むときに見分け、保存のときに戻す）。
//!
//! 読む: BOM（UTF-8・UTF-16）→ UTF-8 として正しいか → 分からないときの文字コード（設定。自動なら Shift_JIS か EUC-JP の
//! 壊れずに読めるほう）。保存: 読んだときの文字コードで書く。その文字コードで表せない文字があれば書かずに断る（中身を壊さない）。

use std::sync::RwLock;

pub use encoding_rs::{EUC_JP, Encoding, SHIFT_JIS, UTF_8, UTF_16BE, UTF_16LE};

/// 分からないときの文字コード（None は自動）。設定の「分からないときの文字コード」をエディタが入れる
static FALLBACK: RwLock<Option<&'static Encoding>> = RwLock::new(None);

pub fn set_fallback(e: Option<&'static Encoding>) {
    if let Ok(mut f) = FALLBACK.write() {
        *f = e;
    }
}

fn fallback() -> Option<&'static Encoding> {
    FALLBACK.read().ok().and_then(|f| *f)
}

/// 名前から（sjis・cp932・euc-jp・utf8・utf-16le・latin1 など。vim の fenc の書き方も）
pub fn by_name(name: &str) -> Option<&'static Encoding> {
    let n = name.trim().to_ascii_lowercase().replace('_', "-");
    Some(match n.as_str() {
        "sjis" | "cp932" | "ms932" | "shift-jis" | "windows-31j" | "shiftjis" => SHIFT_JIS,
        "euc-jp" | "eucjp" | "euc" => EUC_JP,
        "utf-8" | "utf8" => UTF_8,
        "utf-16le" | "utf-16" | "ucs-2le" | "ucs-2" => UTF_16LE,
        "utf-16be" | "ucs-2be" => UTF_16BE,
        "latin1" | "cp1252" => encoding_rs::WINDOWS_1252,
        _ => return Encoding::for_label(n.as_bytes()),
    })
}

/// 画面に出す名前
pub fn label(e: &'static Encoding) -> &'static str {
    if e == SHIFT_JIS { "Shift_JIS" } else { e.name() }
}

/// 読む（中身, 文字コード, BOM があったか）。`forced` ならその文字コードで（:e ++enc=）
pub fn decode(bytes: &[u8], forced: Option<&'static Encoding>) -> (String, &'static Encoding, bool) {
    let boms: [(&[u8], &'static Encoding); 3] = [(b"\xEF\xBB\xBF", UTF_8), (b"\xFF\xFE", UTF_16LE), (b"\xFE\xFF", UTF_16BE)];
    if let Some((b, e)) = boms.iter().find(|(b, e)| bytes.starts_with(b) && forced.is_none_or(|f| f == *e)) {
        return (e.decode_without_bom_handling(&bytes[b.len()..]).0.into_owned(), e, true);
    }
    if let Some(e) = forced {
        return (e.decode_without_bom_handling(bytes).0.into_owned(), e, false);
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return (s.to_string(), UTF_8, false);
    }
    // ほとんどが UTF-8 の多 byte 文字で、壊れた所がわずかなら UTF-8（壊れた所は置き換え）。
    // Shift_JIS・EUC-JP の文字は UTF-8 としてはほぼ読めないので、こうはならない
    let (mut good, mut bad) = (0usize, 0usize);
    for c in bytes.utf8_chunks() {
        good += c.valid().chars().filter(|c| !c.is_ascii()).count();
        bad += usize::from(!c.invalid().is_empty());
    }
    if fallback() == Some(UTF_8) || bad * 20 < good {
        return (String::from_utf8_lossy(bytes).into_owned(), UTF_8, false);
    }
    let cands: Vec<&'static Encoding> = match fallback() {
        Some(e) => vec![e],
        None => vec![SHIFT_JIS, EUC_JP],
    };
    for e in &cands {
        if let Some(s) = e.decode_without_bom_handling_and_without_replacement(bytes) {
            return (s.into_owned(), e, false);
        }
    }
    // どれでも壊れる: 最初のもので読む（壊れた所は置き換え）
    (cands[0].decode_without_bom_handling(bytes).0.into_owned(), cands[0], false)
}

/// 書く。表せない文字があれば、その文字と行で断る
pub fn encode(text: &str, e: &'static Encoding, bom: bool) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len() + 3);
    if e == UTF_16LE || e == UTF_16BE {
        let le = e == UTF_16LE;
        if bom {
            out.extend_from_slice(if le { b"\xFF\xFE" } else { b"\xFE\xFF" });
        }
        for u in text.encode_utf16() {
            out.extend_from_slice(&if le { u.to_le_bytes() } else { u.to_be_bytes() });
        }
        return Ok(out);
    }
    if e == UTF_8 {
        if bom {
            out.extend_from_slice(b"\xEF\xBB\xBF");
        }
        out.extend_from_slice(text.as_bytes());
        return Ok(out);
    }
    let (bytes, _, bad) = e.encode(text);
    if bad {
        let mut buf = [0u8; 4];
        for (no, line) in text.lines().enumerate() {
            if let Some(c) = line.chars().find(|c| e.encode(c.encode_utf8(&mut buf)).2) {
                return Err(format!("{} で表せない文字があります: 「{c}」（{} 行目）。:set fenc=utf-8 で UTF-8 にすれば保存できます", label(e), no + 1));
            }
        }
        return Err(format!("{} で表せない文字があります", label(e)));
    }
    Ok(bytes.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let sjis = SHIFT_JIS.encode("日本語①\r\n").0.into_owned();
        let (s, e, bom) = decode(&sjis, None);
        assert_eq!((s.as_str(), label(e), bom), ("日本語①\r\n", "Shift_JIS", false));
        assert_eq!(encode(&s, e, false).unwrap(), sjis);
        let euc = EUC_JP.encode("日本語").0.into_owned();
        assert_eq!(label(decode(&euc, None).1), "EUC-JP");
        assert_eq!(decode("あ".as_bytes(), None).1, UTF_8);
        let u16 = encode("aあ", UTF_16LE, true).unwrap();
        assert_eq!(decode(&u16, None), ("aあ".to_string(), UTF_16LE, true));
        assert!(encode("😀", SHIFT_JIS, false).unwrap_err().contains("「😀」（1 行目）"));
        assert_eq!(by_name("cp932"), Some(SHIFT_JIS));
        assert_eq!(decode(&sjis, Some(UTF_8)).1, UTF_8, "決めた文字コードで読む");
        let mut broken = "日本語のファイル\n".repeat(50).into_bytes();
        broken.insert(10, 0xFF);
        assert_eq!(decode(&broken, None).1, UTF_8, "UTF-8 の中の壊れた 1 byte では Shift_JIS にしない");
    }
}
