# nagi

A lightweight IDE for Windows with optional Vim keybindings. It looks like a terminal but does more than one:
a GPU-rendered (Direct3D 11) editor with tree-sitter highlighting, go-to-definition without LSP,
project search, git, a terminal panel, run configurations, and database consoles.

- Single executable (`nagi.exe`). No installer.
- Light on memory: about 12 MB for one window. Running `nagi.exe` again opens another window in the same process (about 1.5 MB more) instead of starting a second copy.
- Standard editing keys by default (Shift+arrows to select, Ctrl+C / V / Z, IntelliJ-style shortcuts); everything in the panels also works with the mouse (right-click menus). Vim keybindings are optional (Settings → Vim; reads your `.ideavimrc`).
- Syntax highlighting and go-to-definition for 39 languages:

  | | Languages |
  |---|---|
  | Built in (work offline) | JSON, YAML, INI, XML, Batch, PowerShell, Bash, Markdown |
  | Downloaded automatically the first time you open such a file (a few hundred KB each, verified by SHA-256) | Rust, TypeScript, TSX, JavaScript, Java, Kotlin, Python, Vim script, CSS, HTML, SCSS, C, C++, Go, C#, PHP, Ruby, Lua, Scala, Swift, Dart, Zig, Haskell, Elixir, R, SQL, TOML, Makefile, CMake, Dockerfile, Groovy |

  Once downloaded, a grammar works offline. `:GrammarInstall all` fetches all of them in advance.
- Toolchains (JDK, Python, Node.js, Go, .NET, Rust, clang, CMake, Zig, Dart, PHP) can be picked
  or downloaded per project in Settings (`Ctrl+Alt+S`). Projects in WSL use the tools installed in WSL.

## Download

Get `nagi-<version>-windows-x64.zip` from the [latest release](https://github.com/hashiohiro/nagi-text-editor/releases/latest) and run `nagi.exe` inside.
Windows 10/11 x64.

## Build

```
cargo build --release
```

Requires the MSVC toolchain. The downloadable grammars are built with `grammars/build.sh` (run it before
`cargo test`; tests load the grammars from `target/grammars/dist`).

## License

MIT. See [LICENSE](LICENSE).
Third-party components and their licenses are listed in [THIRD_PARTY_LICENSES.txt](THIRD_PARTY_LICENSES.txt)
(also shown by `:Licenses` in nagi).
