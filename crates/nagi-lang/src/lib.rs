//! 言語の意味（補完・定義へ・参照・import）。画面を持たない（docs/adr/0017）。
//!
//! - `text`: 文字を読む部品（字句の規則 `Lexis` を受け取る）
//! - `eco`: 生態系（ライブラリの索引の持ち主）。jvm（jar・JDK）・dts（TypeScript の .d.ts）
//! - `project`: プロジェクトの索引で型を求める部分
//! - `java`・`ts`: 言語

pub mod eco;
pub mod java;
pub mod project;
pub mod text;
pub mod ts;
