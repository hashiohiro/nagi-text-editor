//! アイコン（IntelliJ IDEA の New UI のアイコン。icons/ の SVG は intellij-community のもので Apache 2.0）。
//!
//! SVG を resvg で描いてアトラスに載せる（大きさと配色ごとに 1 回）。線のアイコンは形だけ（アルファ）を使って文字と同じく
//! 好きな色で塗れ、色のあるアイコン（▶・ファイルの種類・クラス）はそのままの色で出す。配色がライトなら明るい地用の SVG を使う。
//! 名前はファイル名（`tw_project` はツールウィンドウのプロジェクト、`n_class` はクラス、`f_java` は Java のファイル）。

use crate::view::{Instance, Painter};

/// （名前, 明るい地用, 暗い地用）
const ICONS: &[(&str, &str, &str)] = &[
    ("a_preview", include_str!("../icons/a_preview.svg"), include_str!("../icons/a_preview_dark.svg")),
    ("f_css", include_str!("../icons/f_css.svg"), include_str!("../icons/f_css_dark.svg")),
    ("f_docker", include_str!("../icons/f_docker.svg"), include_str!("../icons/f_docker_dark.svg")),
    ("f_gitignore", include_str!("../icons/f_gitignore.svg"), include_str!("../icons/f_gitignore.svg")),
    ("f_html", include_str!("../icons/f_html.svg"), include_str!("../icons/f_html_dark.svg")),
    ("f_image", include_str!("../icons/f_image.svg"), include_str!("../icons/f_image_dark.svg")),
    ("f_javaScript", include_str!("../icons/f_javaScript.svg"), include_str!("../icons/f_javaScript_dark.svg")),
    ("f_java", include_str!("../icons/f_java.svg"), include_str!("../icons/f_java_dark.svg")),
    ("f_json", include_str!("../icons/f_json.svg"), include_str!("../icons/f_json_dark.svg")),
    ("f_markdown", include_str!("../icons/f_markdown.svg"), include_str!("../icons/f_markdown_dark.svg")),
    ("f_properties", include_str!("../icons/f_properties.svg"), include_str!("../icons/f_properties_dark.svg")),
    ("f_shell", include_str!("../icons/f_shell.svg"), include_str!("../icons/f_shell_dark.svg")),
    ("f_sql", include_str!("../icons/f_sql.svg"), include_str!("../icons/f_sql_dark.svg")),
    ("f_text", include_str!("../icons/f_text.svg"), include_str!("../icons/f_text_dark.svg")),
    ("f_typeScript", include_str!("../icons/f_typeScript.svg"), include_str!("../icons/f_typeScript_dark.svg")),
    ("f_unknown", include_str!("../icons/f_unknown.svg"), include_str!("../icons/f_unknown_dark.svg")),
    ("f_xml", include_str!("../icons/f_xml.svg"), include_str!("../icons/f_xml_dark.svg")),
    ("f_yaml", include_str!("../icons/f_yaml.svg"), include_str!("../icons/f_yaml_dark.svg")),
    ("g_chevronDown", include_str!("../icons/g_chevronDown.svg"), include_str!("../icons/g_chevronDown_dark.svg")),
    ("g_closeSmall", include_str!("../icons/g_closeSmall.svg"), include_str!("../icons/g_closeSmall_dark.svg")),
    ("g_close", include_str!("../icons/g_close.svg"), include_str!("../icons/g_close_dark.svg")),
    ("g_hide", include_str!("../icons/g_hide.svg"), include_str!("../icons/g_hide_dark.svg")),
    ("g_informationDialog", include_str!("../icons/g_informationDialog.svg"), include_str!("../icons/g_informationDialog_dark.svg")),
    ("g_warningDialog", include_str!("../icons/g_warningDialog.svg"), include_str!("../icons/g_warningDialog_dark.svg")),
    ("g_errorDialog", include_str!("../icons/g_errorDialog.svg"), include_str!("../icons/g_errorDialog_dark.svg")),
    ("g_questionDialog", include_str!("../icons/g_questionDialog.svg"), include_str!("../icons/g_questionDialog_dark.svg")),
    ("g_moreVertical", include_str!("../icons/g_moreVertical.svg"), include_str!("../icons/g_moreVertical_dark.svg")),
    ("g_search", include_str!("../icons/g_search.svg"), include_str!("../icons/g_search_dark.svg")),
    ("g_settings", include_str!("../icons/g_settings.svg"), include_str!("../icons/g_settings_dark.svg")),
    ("g_vcs", include_str!("../icons/g_vcs.svg"), include_str!("../icons/g_vcs_dark.svg")),
    ("n_annotation", include_str!("../icons/n_annotation.svg"), include_str!("../icons/n_annotation_dark.svg")),
    ("n_class", include_str!("../icons/n_class.svg"), include_str!("../icons/n_class_dark.svg")),
    ("n_dataSchema", include_str!("../icons/n_dataSchema.svg"), include_str!("../icons/n_dataSchema_dark.svg")),
    ("n_enum", include_str!("../icons/n_enum.svg"), include_str!("../icons/n_enum_dark.svg")),
    ("n_excludeRoot", include_str!("../icons/n_excludeRoot.svg"), include_str!("../icons/n_excludeRoot_dark.svg")),
    ("n_field", include_str!("../icons/n_field.svg"), include_str!("../icons/n_field_dark.svg")),
    ("n_folder", include_str!("../icons/n_folder.svg"), include_str!("../icons/n_folder_dark.svg")),
    ("n_interface", include_str!("../icons/n_interface.svg"), include_str!("../icons/n_interface_dark.svg")),
    ("n_method", include_str!("../icons/n_method.svg"), include_str!("../icons/n_method_dark.svg")),
    ("n_record", include_str!("../icons/n_record.svg"), include_str!("../icons/n_record_dark.svg")),
    ("n_resourcesRoot", include_str!("../icons/n_resourcesRoot.svg"), include_str!("../icons/n_resourcesRoot_dark.svg")),
    ("n_sourceRoot", include_str!("../icons/n_sourceRoot.svg"), include_str!("../icons/n_sourceRoot_dark.svg")),
    ("n_testRoot", include_str!("../icons/n_testRoot.svg"), include_str!("../icons/n_testRoot_dark.svg")),
    ("r_debug", include_str!("../icons/r_debug.svg"), include_str!("../icons/r_debug_dark.svg")),
    ("r_run", include_str!("../icons/r_run.svg"), include_str!("../icons/r_run_dark.svg")),
    ("r_stop", include_str!("../icons/r_stop.svg"), include_str!("../icons/r_stop_dark.svg")),
    ("tw_commit", include_str!("../icons/tw_commit.svg"), include_str!("../icons/tw_commit_dark.svg")),
    ("tw_debug", include_str!("../icons/tw_debug.svg"), include_str!("../icons/tw_debug_dark.svg")),
    ("tw_find", include_str!("../icons/tw_find.svg"), include_str!("../icons/tw_find_dark.svg")),
    ("tw_maven", include_str!("../icons/tw_maven.svg"), include_str!("../icons/tw_maven_dark.svg")),
    ("tw_messages", include_str!("../icons/tw_messages.svg"), include_str!("../icons/tw_messages_dark.svg")),
    ("tw_notifications", include_str!("../icons/tw_notifications.svg"), include_str!("../icons/tw_notifications_dark.svg")),
    ("tw_problems", include_str!("../icons/tw_problems.svg"), include_str!("../icons/tw_problems_dark.svg")),
    ("tw_project", include_str!("../icons/tw_project.svg"), include_str!("../icons/tw_project_dark.svg")),
    ("tw_run", include_str!("../icons/tw_run.svg"), include_str!("../icons/tw_run_dark.svg")),
    ("tw_structure", include_str!("../icons/tw_structure.svg"), include_str!("../icons/tw_structure_dark.svg")),
    ("tw_terminal", include_str!("../icons/tw_terminal.svg"), include_str!("../icons/tw_terminal_dark.svg")),
    ("tw_vcs", include_str!("../icons/tw_vcs.svg"), include_str!("../icons/tw_vcs_dark.svg")),
];

/// アトラスの鍵の face（フォントの face と重ならない値）
const FACE: u16 = 0xfff0;

fn index(name: &str) -> Option<usize> {
    ICONS.iter().position(|i| i.0 == name)
}

/// SVG を `px` 四方の画像にする（`mono` なら形のアルファだけ）
fn rasterize(svg: &str, px: u32, mono: bool) -> Option<nagi_text::GlyphImage> {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).ok()?;
    let mut pix = resvg::tiny_skia::Pixmap::new(px, px)?;
    let size = tree.size();
    let scale = px as f32 / size.width().max(size.height());
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pix.as_mut());
    // tiny-skia は乗算済みの RGBA。描くほうは乗算していない色を使う
    let data: Vec<u8> = if mono {
        pix.pixels().iter().map(|c| c.alpha()).collect()
    } else {
        pix.pixels().iter().flat_map(|c| {
            let c = c.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        }).collect()
    };
    Some(nagi_text::GlyphImage { width: px, height: px, left: 0, top: 0, color: !mono, data })
}

/// アイコンを (x, y) から `px` 四方に描く。`tint` があれば形をその色で塗り、無ければ SVG の色のまま
pub(crate) fn draw(p: &mut Painter, name: &str, x: f32, y: f32, px: f32, tint: Option<[f32; 4]>) {
    let Some(i) = index(name) else { return };
    let px = px.round().clamp(8.0, 64.0) as u32;
    let dark = crate::view::THEME.load(std::sync::atomic::Ordering::Relaxed) != 2;
    let mono = tint.is_some();
    let key = nagi_text::GlyphKey { face: FACE, glyph: (i as u16) | (u16::from(dark) << 14) | (u16::from(mono) << 15), width: px as u8 };
    let svg = if dark { ICONS[i].2 } else { ICONS[i].1 };
    let Some(a) = p.r.image(key, || rasterize(svg, px, mono)) else { return };
    let (x, y) = (x.round(), y.round());
    let color = tint.unwrap_or([1.0, 1.0, 1.0, 1.0]);
    p.out.push(Instance { rect: [x, y, a.uv[2], a.uv[3]], uv: a.uv, color, kind: if a.color { 2 } else { 1 } });
}

/// 行の高さに対する、ふつうのアイコン（16）の大きさ（IntelliJ と同じく行より少し小さく）
pub(crate) fn small(cell_h: f32) -> f32 {
    (cell_h * 0.8).round()
}

/// ツールウィンドウの列・上の帯のアイコン（20）の大きさ
pub(crate) fn large(cell_h: f32) -> f32 {
    (cell_h * 1.0).round()
}

/// セルの行 `y` の中で、`px` のアイコンを縦の真ん中に置く y
pub(crate) fn centered(y: f32, cell_h: f32, px: f32) -> f32 {
    y + ((cell_h - px) / 2.0).round()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// どのアイコンも読めて、描くと何か出る（暗い地用・明るい地用とも）
    #[test]
    fn all_icons_render() {
        for (name, light, dark) in ICONS {
            for svg in [light, dark] {
                let img = rasterize(svg, 24, false).unwrap_or_else(|| panic!("{name}"));
                assert!(img.data.chunks(4).any(|c| c[3] > 0), "{name} が空");
                let m = rasterize(svg, 24, true).unwrap();
                assert_eq!(m.data.len(), 24 * 24);
            }
        }
    }
}
