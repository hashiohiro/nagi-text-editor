//! IdeaVim の `:action <IntelliJ のアクション名>` を、Nagi の機能（コマンド名）に対応させる表。
//!
//! Nagi に相当する機能があるものはそのコマンド名を、無いもの（LSP なしの方針で持たない機能など）は
//! `None` を返す。コマンドの実装はマイルストーンごとに増やしていき、未実装のものはエディタが「未実装」と表示する。

/// IntelliJ のアクション名 → (Nagi のコマンド名, 説明)。`None` は Nagi に相当する機能が無い
pub fn ideavim(name: &str) -> Option<(Option<&'static str>, &'static str)> {
    let r = match name {
        // 検索・置換・ファインダー（M2, M4）
        "Find" => (Some("search.buffer"), "ファイル内検索"),
        "Replace" => (Some("replace.buffer"), "ファイル内置換"),
        "SearchEverywhere" => (Some("finder.everything"), "すべてを検索"),
        "FindInPath" => (Some("search.project"), "プロジェクト全文検索"),
        "GotoAction" => (Some("palette"), "コマンドパレット"),
        "GotoFile" => (Some("finder.files"), "ファイルを開く"),
        "GotoSymbol" => (Some("finder.symbols"), "シンボルへ移動（tree-sitter の索引）"),
        "GotoClass" => (Some("finder.types"), "型へ移動（tree-sitter の索引）"),
        "RecentFiles" => (Some("finder.recent"), "最近開いたファイル"),
        "RecentLocations" => (Some("jump.recent"), "最近の編集箇所"),
        "ManageRecentProjects" => (Some("finder.projects"), "最近のプロジェクト"),
        "Markdown.Layout.EditorAndPreview" | "Markdown.Layout.PreviewOnly" | "Markdown.Layout.EditorOnly" => (Some("markdown.preview"), "Markdown のプレビュー（右の欄）を出す・閉じる"),
        "FileStructurePopup" => (Some("outline"), "ファイルの構造（tree-sitter）"),
        "GotoDeclaration" | "GotoTypeDeclaration" => (Some("nav.definition"), "定義へ（tree-sitter の索引）"),
        "GotoImplementation" => (Some("nav.implementation"), "実装へ（継承・実装している型の中）"),
        "FindUsages" | "ShowUsages" => (Some("nav.references"), "参照を探す（名前の一致）"),
        "VcsShowNextChangeMarker" => (Some("git.hunk_next"), "次の変更へ"),
        "VcsShowPrevChangeMarker" => (Some("git.hunk_prev"), "前の変更へ"),
        "VcsShowCurrentChangeMarker" => (Some("git.hunk_popup"), "カーソルの変更を見る"),
        "Vcs.RollbackChangedLines" => (Some("git.hunk_revert"), "カーソルの変更を戻す"),
        // 移動（M3, M4）
        "Back" => (Some("jump.back"), "前の位置へ（Ctrl-o 相当）"),
        "Forward" => (Some("jump.forward"), "次の位置へ（Ctrl-i 相当）"),
        "JumpToLastChange" => (Some("jump.last_change"), "最後の変更箇所へ（g; 相当）"),
        "MethodUp" => (Some("nav.method_prev"), "前のメソッドへ（tree-sitter）"),
        "MethodDown" => (Some("nav.method_next"), "次のメソッドへ（tree-sitter）"),
        "GotoPreviousError" => (Some("nav.error_prev"), "前の構文エラーへ（tree-sitter の ERROR ノード）"),
        "GotoNextError" => (Some("nav.error_next"), "次の構文エラーへ（tree-sitter の ERROR ノード）"),
        "EasymotionWordForward" => (Some("jump.word"), "EasyMotion: 単語へジャンプ"),
        "EasymotionLineForward" => (Some("jump.line_down"), "EasyMotion: 下の行へジャンプ"),
        "EasymotionLineBackward" => (Some("jump.line_up"), "EasyMotion: 上の行へジャンプ"),
        "EasymotionSearch" => (Some("jump.search"), "EasyMotion: 文字列へジャンプ"),
        "EasymotionFindChar" => (Some("jump.char"), "EasyMotion: 1 文字へジャンプ"),
        // ツールウィンドウ・バッファ（M2）
        "CloseContent" => (Some("buffer.close"), "バッファを閉じる"),
        "ReopenClosedTab" => (Some("buffer.reopen"), "閉じたバッファを開き直す"),
        "ActivateTerminalToolWindow" => (Some("tool.terminal"), "ターミナルの表示切替"),
        "ActivateProjectToolWindow" => (Some("tool.project"), "プロジェクトの表示切替"),
        "HideAllWindows" => (Some("tool.hide_all"), "ツールウィンドウをすべて隠す"),
        // 整形・実行（外部コマンド。M4）
        "ReformatCode" => (Some("format"), "整形（外部フォーマッタ）"),
        "Run" => (Some("job.run"), "実行（設定したコマンド）"),
        "RunContextTest" => (Some("job.test"), "カーソル位置のテストを実行"),
        "Stop" => (Some("job.stop"), "実行中のコマンドを止める"),
        // git（M5）
        "Vcs.ShowTabbedFileHistory" => (Some("git.file_history"), "ファイルの履歴"),
        "Vcs.Show.Log" | "ActivateVersionControlToolWindow" => (Some("git.log"), "履歴（グラフ付きのログ）"),
        "Annotate" => (Some("git.blame"), "blame"),
        "ChangesView" => (Some("git.changes"), "変更の一覧（コミットのパネル）"),
        "CheckinProject" => (Some("git.commit"), "コミット（コミットのパネル）"),
        "Vcs.Push" => (Some("git.push"), "push"),
        "Vcs.UpdateProject" => (Some("git.pull"), "pull"),
        "Git.Branches" => (Some("git.branches"), "ブランチ"),
        "Git.Fetch" => (Some("git.fetch"), "フェッチ"),
        // Nagi に相当する機能が無い（LSP・IDE 固有）
        "OptimizeImports" => (None, "import の整理（LSP なしのため対象外）"),
        "QuickJavaDoc" => (None, "ドキュメント表示（LSP なしのため対象外）"),
        "ShowIntentionActions" => (None, "インテンション（LSP なしのため対象外）"),
        "Generate" | "NewClass" | "OverrideMethods" | "GenerateConstructor" | "GenerateGetter" | "GenerateSetter"
        | "GenerateGetterAndSetter" | "GenerateEquals" | "GenerateTestMethod" => (None, "コード生成（IntelliJ 固有のため対象外）"),
        "Debug" | "DebugContextTest" | "ToggleLineBreakpoint" => (None, "デバッガ（スコープ外）"),
        "ActivateMavenToolWindow" => (Some("tool.maven"), "Maven（右の欄）"),
        "ActivateDatabaseToolWindow" => (Some("tool.database"), "データベース（右の欄）"),
        "Console.Jdbc.OpenConsole" | "DatabaseView.NewQueryConsole" => (Some("db.new_query"), "新しいクエリ"),
        _ => return None,
    };
    Some(r)
}
