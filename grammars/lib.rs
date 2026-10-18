//! 文法を 1 つだけ入れた DLL。`nagi_grammar()` が tree-sitter の言語（TSLanguage）を返す。
//! nagi は LoadLibrary で読み込み、この関数を tree_sitter_language::LanguageFn にして使う（nagi-syntax の grammar.rs）。

/// `feature` が有効なら、その文法を返す `nagi_grammar` を作る
macro_rules! grammar {
    ($feature:literal, $language:expr) => {
        #[cfg(feature = $feature)]
        #[unsafe(no_mangle)]
        pub extern "C" fn nagi_grammar() -> *const () {
            let f: tree_sitter_language::LanguageFn = $language;
            // SAFETY: 文法の C の関数を呼ぶだけ（引数なし、静的な表を指すポインタを返す）
            unsafe { (f.into_raw())() }
        }
    };
}

grammar!("rust", tree_sitter_rust::LANGUAGE);
grammar!("typescript", tree_sitter_typescript::LANGUAGE_TYPESCRIPT);
grammar!("tsx", tree_sitter_typescript::LANGUAGE_TSX);
grammar!("javascript", tree_sitter_javascript::LANGUAGE);
grammar!("java", tree_sitter_java::LANGUAGE);
grammar!("kotlin", tree_sitter_kotlin_ng::LANGUAGE);
grammar!("python", tree_sitter_python::LANGUAGE);
grammar!("css", tree_sitter_css::LANGUAGE);
grammar!("html", tree_sitter_html::LANGUAGE);
grammar!("c", tree_sitter_c::LANGUAGE);
grammar!("cpp", tree_sitter_cpp::LANGUAGE);
grammar!("go", tree_sitter_go::LANGUAGE);
grammar!("csharp", tree_sitter_c_sharp::LANGUAGE);
grammar!("php", tree_sitter_php::LANGUAGE_PHP);
grammar!("ruby", tree_sitter_ruby::LANGUAGE);
grammar!("lua", tree_sitter_lua::LANGUAGE);
grammar!("scala", tree_sitter_scala::LANGUAGE);
grammar!("swift", tree_sitter_swift::LANGUAGE);
grammar!("dart", tree_sitter_dart::LANGUAGE);
grammar!("zig", tree_sitter_zig::LANGUAGE);
grammar!("haskell", tree_sitter_haskell::LANGUAGE);
grammar!("elixir", tree_sitter_elixir::LANGUAGE);
grammar!("r", tree_sitter_r::LANGUAGE);
grammar!("sql", tree_sitter_sequel::LANGUAGE);
grammar!("toml", tree_sitter_toml_ng::LANGUAGE);
grammar!("xml", tree_sitter_xml::LANGUAGE_XML);
grammar!("make", tree_sitter_make::LANGUAGE);
grammar!("cmake", tree_sitter_cmake::LANGUAGE);
grammar!("groovy", tree_sitter_groovy::LANGUAGE);

// tree-sitter-dockerfile 0.2.0 は古い形式（LANGUAGE の定数が無い）。C の関数をそのまま返す
#[cfg(feature = "dockerfile")]
extern crate tree_sitter_dockerfile;
#[cfg(feature = "dockerfile")]
unsafe extern "C" {
    fn tree_sitter_dockerfile() -> *const ();
}
#[cfg(feature = "dockerfile")]
#[unsafe(no_mangle)]
pub extern "C" fn nagi_grammar() -> *const () {
    // SAFETY: 同上
    unsafe { tree_sitter_dockerfile() }
}

/// 古い形式のバインディング（LANGUAGE の定数が無い）の文法は、C の関数をそのまま返す
macro_rules! raw_grammar {
    ($feature:literal, $krate:ident, $func:ident) => {
        #[cfg(feature = $feature)]
        extern crate $krate;
        #[cfg(feature = $feature)]
        unsafe extern "C" {
            fn $func() -> *const ();
        }
        #[cfg(feature = $feature)]
        #[unsafe(no_mangle)]
        pub extern "C" fn nagi_grammar() -> *const () {
            // SAFETY: 同上
            unsafe { $func() }
        }
    };
}

raw_grammar!("vim", tree_sitter_vim, tree_sitter_vim);
raw_grammar!("scss", tree_sitter_scss, tree_sitter_scss);
