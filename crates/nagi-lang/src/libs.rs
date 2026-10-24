//! ライブラリの索引の入れ物（生態系ごとに 1 つ。読み込むのは生態系、使うのは言語）

use std::any::Any;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use nagi_core::task::{Task, Waker};

use crate::eco::{ALL, Ecosystem, Loaded};

/// 生態系の名前 → 読み込んだ索引（と、読み込んでいる途中のもの）
#[derive(Default)]
pub struct Libraries {
    slots: HashMap<&'static str, Arc<dyn Any + Send + Sync>>,
    loading: Vec<(&'static str, Task<Result<Loaded, String>>)>,
}

impl Libraries {
    /// 生態系 `eco` の索引（型 T として）
    pub fn get<T: Any + Send + Sync>(&self, eco: &str) -> Option<Arc<T>> {
        self.slots.get(eco).cloned().and_then(|a| a.downcast::<T>().ok())
    }

    pub fn set<T: Any + Send + Sync>(&mut self, eco: &'static str, v: Arc<T>) {
        self.slots.insert(eco, v);
    }

    pub fn has(&self, eco: &str) -> bool {
        self.slots.contains_key(eco)
    }

    /// プロジェクトの根 `root` で要る生態系の索引を、裏で読み込み始める（`only` があればそれを作り直すきっかけにする生態系だけ）
    pub fn start(&mut self, root: &Path, only: Option<&Path>, waker: Waker) {
        for eco in ALL.iter().filter(|e| only.is_none_or(|p| e.rebuild_on(p))) {
            if let Some(r) = eco.root(root) {
                self.spawn(*eco, r, waker.clone());
            }
        }
    }

    fn spawn(&mut self, eco: &'static dyn Ecosystem, root: std::path::PathBuf, waker: Waker) {
        let name = eco.name();
        self.loading.retain(|(n, _)| *n != name);
        self.loading.push((name, nagi_core::task::spawn(name, waker, move |ctx| {
            ctx.send(eco.load(&root));
        })));
    }

    /// ファイル `path` が、どれかの生態系の索引を作り直すきっかけか
    pub fn rebuild_on(&self, path: &Path) -> bool {
        ALL.iter().any(|e| e.rebuild_on(path))
    }

    /// 読み込み終わったものを受け取る。(読み込んだか, 記録に残すこと) の並び
    pub fn poll(&mut self) -> Vec<Result<String, String>> {
        let mut out = Vec::new();
        let mut k = 0;
        while k < self.loading.len() {
            match self.loading[k].1.try_recv() {
                Ok(r) => {
                    let (name, _) = self.loading.remove(k);
                    match r {
                        Ok(l) => {
                            self.slots.insert(name, l.index);
                            out.push(Ok(l.log));
                        }
                        Err(e) => out.push(Err(format!("{name}: {e}"))),
                    }
                }
                // 読み込む処理が落ちた
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    let (name, _) = self.loading.remove(k);
                    out.push(Err(format!("{name}: 読み込みが途中で終わりました")));
                }
                Err(_) => k += 1,
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// node_modules に typescript があるプロジェクトで JS の索引を読み込み、package.json で作り直す（pom.xml では作り直さない）
    #[test]
    fn loads_by_ecosystem() {
        let dir = std::env::temp_dir().join(format!("nagi-libs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("node_modules/typescript/lib")).unwrap();
        std::fs::write(dir.join("node_modules/typescript/lib/lib.es5.d.ts"), "interface Array<T> { length: number; }\n").unwrap();
        std::fs::write(dir.join("tsconfig.json"), "{}").unwrap();
        let mut l = Libraries::default();
        let wait = |l: &mut Libraries| {
            for _ in 0..2000 {
                let r = l.poll();
                if !r.is_empty() {
                    return r;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            Vec::new()
        };
        l.start(&dir, None, nagi_core::task::no_waker());
        assert!(matches!(wait(&mut l).as_slice(), [Ok(m)] if m.contains("TypeScript")));
        assert!(crate::eco::dts(&l).is_some() && crate::eco::jars(&l).is_none(), "Maven でないので JVM は読まない");
        assert!(l.rebuild_on(Path::new("x/package.json")) && l.rebuild_on(Path::new("pom.xml")) && !l.rebuild_on(Path::new("a.ts")));
        l.start(&dir, Some(Path::new("pom.xml")), nagi_core::task::no_waker());
        assert!(l.loading.is_empty(), "pom.xml では JS を作り直さない");
        l.start(&dir, Some(&dir.join("package.json")), nagi_core::task::no_waker());
        assert_eq!(l.loading.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
