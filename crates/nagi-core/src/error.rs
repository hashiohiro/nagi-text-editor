//! エラーの型（ADR 0016 の 3）。
//!
//! ```ignore
//! fn load(p: &Path) -> nagi_core::Result<String> {
//!     std::fs::read_to_string(p).context(format!("{} を読めません", p.display()))
//! }
//! ```
//!
//! - 原因の連なり（cause）を持つ。表示（Display）は「文脈: 原因: 原因…」の 1 行で、利用者に出す文はこれ 1 か所で作る。
//! - String・&str・io::Error から `?` で変換できる（`Err(format!(…))?` もそのまま使える）。
//! - 依存を足さないため anyhow などは使わない（ADR 0016）。

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    msg: String,
    cause: Option<Box<Error>>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(msg: impl Into<String>) -> Self {
        Self { msg: msg.into(), cause: None }
    }

    /// 表示できるもの（ほかのクレートのエラーなど）から
    pub fn of(e: impl fmt::Display) -> Self {
        Self::new(e.to_string())
    }

    /// この段の文（原因を含まない）
    pub fn msg(&self) -> &str {
        &self.msg
    }

    /// いちばん奥の原因
    pub fn root(&self) -> &Error {
        let mut e = self;
        while let Some(c) = &e.cause {
            e = c;
        }
        e
    }

    /// 文脈を外側に足す
    pub fn wrap(self, msg: impl Into<String>) -> Self {
        Self { msg: msg.into(), cause: Some(Box::new(self)) }
    }

    /// 全体の文の最後の行（git などの長い出力の、いちばん大事な行）
    pub fn last_line(&self) -> String {
        self.to_string().lines().last().unwrap_or("").to_string()
    }

    /// 中身が無いか（「まだ」の印に使う空のエラー）
    pub fn is_empty(&self) -> bool {
        self.msg.is_empty() && self.cause.is_none()
    }

    /// 全体の文の最初の行
    pub fn first_line(&self) -> String {
        self.to_string().lines().next().unwrap_or("").to_string()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)?;
        if let Some(c) = &self.cause {
            if self.msg.is_empty() {
                write!(f, "{c}")?;
            } else {
                write!(f, ": {c}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

/// 利用者に出す文として（知らせ・画面に渡す）
impl From<Error> for String {
    fn from(e: Error) -> String {
        e.to_string()
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::of(e)
    }
}

/// 文脈を足す（Result と Option に）
pub trait Context<T> {
    fn context(self, msg: impl Into<String>) -> Result<T>;
    fn with_context(self, f: impl FnOnce() -> String) -> Result<T>;
}

/// 表示できるエラーなら何にでも（ほかのクレートのエラー・String・Error）。原因の文は保たれる
impl<T, E: fmt::Display> Context<T> for std::result::Result<T, E> {
    fn context(self, msg: impl Into<String>) -> Result<T> {
        self.map_err(|e| Error::of(e).wrap(msg))
    }

    fn with_context(self, f: impl FnOnce() -> String) -> Result<T> {
        self.map_err(|e| Error::of(e).wrap(f()))
    }
}

impl<T> Context<T> for Option<T> {
    fn context(self, msg: impl Into<String>) -> Result<T> {
        self.ok_or_else(|| Error::new(msg))
    }

    fn with_context(self, f: impl FnOnce() -> String) -> Result<T> {
        self.ok_or_else(|| Error::new(f()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(p: &str) -> Result<String> {
        Ok(std::fs::read_to_string(p).context(format!("{p} を読めません"))?)
    }

    #[test]
    fn chain_and_display() {
        let e = read("Z:/無いところ/x.txt").unwrap_err();
        assert!(e.to_string().starts_with("Z:/無いところ/x.txt を読めません: "), "{e}");
        assert_eq!(e.msg(), "Z:/無いところ/x.txt を読めません");
        assert_ne!(e.root().msg(), e.msg());
        let e: Error = "一行目\n最後の行".into();
        assert_eq!(e.last_line(), "最後の行");
        let n: Option<u8> = None;
        assert_eq!(n.context("ありません").unwrap_err().to_string(), "ありません");
        let f = || -> Result<()> { Err(format!("{} 件", 3))? };
        assert_eq!(f().unwrap_err().to_string(), "3 件", "String から ? で変換できる");
    }
}
