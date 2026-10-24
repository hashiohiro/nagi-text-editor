//! 生態系（ライブラリの索引の持ち主）。Java と Kotlin は jvm を、TypeScript と JavaScript は dts を使う

use std::sync::Arc;

use crate::libs::Libraries;

pub mod dts;
pub mod jvm;

/// JVM（jar・JDK）の索引の名前
pub const JVM: &str = "jvm";
/// JS（組み込みと node_modules の .d.ts）の索引の名前
pub const JS: &str = "js";

pub fn jars(l: &Libraries) -> Option<Arc<jvm::JarIndex>> {
    l.get(JVM)
}

pub fn dts(l: &Libraries) -> Option<Arc<dts::DtsIndex>> {
    l.get(JS)
}
