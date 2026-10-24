//! ライブラリの索引の入れ物（生態系ごとに 1 つ。読み込むのは生態系、使うのは言語）

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

/// 生態系の名前 → 読み込んだ索引
#[derive(Default, Clone)]
pub struct Libraries {
    slots: HashMap<&'static str, Arc<dyn Any + Send + Sync>>,
}

impl Libraries {
    /// 生態系 `eco` の索引（型 T として）
    pub fn get<T: Any + Send + Sync>(&self, eco: &str) -> Option<Arc<T>> {
        self.slots.get(eco).cloned().and_then(|a| a.downcast::<T>().ok())
    }

    pub fn set<T: Any + Send + Sync>(&mut self, eco: &'static str, v: Arc<T>) {
        self.slots.insert(eco, v);
    }

    pub fn remove(&mut self, eco: &str) {
        self.slots.remove(eco);
    }

    pub fn has(&self, eco: &str) -> bool {
        self.slots.contains_key(eco)
    }
}
