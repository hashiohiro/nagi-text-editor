//! 応答の速さの計測（`:NagiStats`）。キー 1 打ごとに「受け取ってから Present を終えるまで」を内訳つきで記録する。
//!
//! 測っているのは nagi の中だけ。OS がキーを届けるまでと、Present の後に画面へ出るまで（合成・走査）は含まない。
//! そこまで含めた値は PresentMon で測る（docs/spikes/s3-s4-result.md）。

use std::time::Duration;

/// 直近の記録数
const CAP: usize = 512;

/// 1 打分の内訳（µs）
#[derive(Clone, Copy, Default)]
pub struct Sample {
    /// エディタの処理（キーの解釈・編集・構文木の更新）
    pub edit: u32,
    /// スワップチェーンの待ち（前のフレームが画面に出るのを待つ）
    pub wait: u32,
    /// 画面の組み立て
    pub build: u32,
    /// GPU への送出と Present
    pub present: u32,
}

impl Sample {
    pub fn total(&self) -> u32 {
        self.edit + self.wait + self.build + self.present
    }
}

#[derive(Default)]
pub struct Stats {
    ring: Vec<Sample>,
    next: usize,
}

pub fn us(d: Duration) -> u32 {
    d.as_micros().min(u32::MAX as u128) as u32
}

impl Stats {
    pub fn push(&mut self, s: Sample) {
        if self.ring.len() < CAP {
            self.ring.push(s);
        } else {
            self.ring[self.next] = s;
        }
        self.next = (self.next + 1) % CAP;
    }

    pub fn clear(&mut self) {
        self.ring.clear();
        self.next = 0;
    }

    /// 最下行に出す 1 行
    pub fn summary(&self, startup_ms: Option<f64>, memory_mb: f64) -> String {
        let startup = startup_ms.map_or(String::new(), |t| format!("  起動 {t:.0}ms"));
        let tail = format!("{startup}  メモリ {memory_mb:.1}MB");
        if self.ring.is_empty() {
            return format!("キー→表示: まだ記録がありません{tail}");
        }
        let mut totals: Vec<u32> = self.ring.iter().map(Sample::total).collect();
        totals.sort_unstable();
        let pct = |p: f64| totals[((totals.len() - 1) as f64 * p).round() as usize];
        let n = self.ring.len() as u64;
        let avg = |f: fn(&Sample) -> u32| self.ring.iter().map(|s| f(s) as u64).sum::<u64>() as f64 / n as f64 / 1000.0;
        let ms = |v: u32| v as f64 / 1000.0;
        format!(
            "キー→表示 中央 {:.2} / 99% {:.2} / 最大 {:.2}ms（{n}打） 内訳 処理 {:.2} 待ち {:.2} 組立 {:.2} 送出 {:.2}{tail}",
            ms(pct(0.5)),
            ms(pct(0.99)),
            ms(*totals.last().unwrap()),
            avg(|s| s.edit),
            avg(|s| s.wait),
            avg(|s| s.build),
            avg(|s| s.present),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_and_ring() {
        let mut s = Stats::default();
        for i in 1..=1000u32 {
            s.push(Sample { edit: i, ..Default::default() });
        }
        assert_eq!(s.ring.len(), CAP);
        // 残っているのは直近 512 打（489..=1000µs）
        let line = s.summary(Some(50.0), 24.0);
        assert!(line.contains("中央 0.74 "), "{line}");
        assert!(line.contains("最大 1.00ms"), "{line}");
        assert!(line.contains("起動 50ms"), "{line}");
    }
}
