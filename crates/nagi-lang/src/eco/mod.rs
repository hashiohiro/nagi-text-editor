//! 生態系（ライブラリの索引の持ち主）。Java と Kotlin は jvm を、TypeScript と JavaScript は dts を使う

use std::any::Any;
use std::path::{Path, PathBuf};
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

/// 読み込んだ索引と、記録に残すこと
pub struct Loaded {
    pub index: Arc<dyn Any + Send + Sync>,
    pub log: String,
}

/// 生態系（ライブラリの索引の持ち主）
pub trait Ecosystem: Sync {
    /// 索引の名前（Libraries の鍵）
    fn name(&self) -> &'static str;

    /// プロジェクトの根 `root` で要るなら、読み込む根（Maven の親の pom.xml のあるフォルダなど）
    fn root(&self, root: &Path) -> Option<PathBuf>;

    /// 読み込む（裏のスレッドで呼ぶ）
    fn load(&self, root: &Path) -> Result<Loaded, String>;

    /// 保存・外で変わったファイルが、作り直すきっかけか（pom.xml・package.json）
    fn rebuild_on(&self, path: &Path) -> bool;

    /// 作り直すきっかけのファイルを、定義の索引にも入れるか（package.json は入れる、pom.xml は入れない）
    fn index_build_file(&self) -> bool {
        true
    }

    /// 索引を作るときに見ない場所にある、作り直すきっかけのファイル（根からの相対パス。`.mvn/maven.config`）
    fn hidden_build_files(&self) -> &'static [&'static str] {
        &[]
    }
}

/// 索引に教える、ビルドの設定のファイル（すべての生態系の作り直すきっかけ）
pub fn build_files() -> nagi_workers::index::BuildFiles {
    nagi_workers::index::BuildFiles {
        is: |p| ALL.iter().find(|e| e.rebuild_on(p)).map(|e| e.index_build_file()),
        extra: ALL.iter().flat_map(|e| e.hidden_build_files()).map(|s| s.to_string()).collect(),
    }
}

/// すべての生態系
pub static ALL: &[&dyn Ecosystem] = &[&jvm::ECO, &dts::ECO];
