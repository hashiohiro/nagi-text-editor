# nagi

A lightweight IDE for Windows with Vim keybindings. It looks like a terminal but does more than one:
a GPU-rendered (Direct3D 11) editor with tree-sitter highlighting, go-to-definition without LSP,
project search, git, a terminal panel, run configurations, and database consoles.

- Single executable (`nagi.exe`). No installer.
- Vim keybindings (reads your `.ideavimrc`).
- Languages built in: Rust, TypeScript/TSX, JavaScript, Java, Kotlin, Python, Bash, Batch, PowerShell,
  INI, JSON, YAML, Vim script, CSS, SCSS, HTML, Markdown.
- Languages downloaded on first use (from the `grammars-v1` release, verified by SHA-256):
  C, C++, Go, C#, PHP, Ruby, Lua, Scala, Swift, Dart, Zig, Haskell, Elixir, R, SQL, TOML, XML,
  Makefile, CMake, Dockerfile, Groovy. Use `:GrammarInstall all` to fetch them in advance.
- Toolchains (JDK, Python, Node.js, Go, .NET, Rust, clang, CMake, Zig, Dart, PHP) can be picked
  or downloaded per project in Settings (`Ctrl+Alt+S`). Projects in WSL use the tools installed in WSL.

## Download

Get `nagi.exe` from the [latest release](https://github.com/hashiohiro/nagi-text-editor/releases/latest).
Windows 10/11 x64.

## Build

```
cargo build --release
```

Requires the MSVC toolchain. The downloadable grammars are built with `grammars/build.sh`.

## License

MIT. See [LICENSE](LICENSE).
Third-party components and their licenses are listed in [THIRD_PARTY_LICENSES.txt](THIRD_PARTY_LICENSES.txt)
(also shown by `:Licenses` in nagi).
