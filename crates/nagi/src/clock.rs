//! 起動計測: プロセス生成時刻（GetProcessTimes）からの経過を記録する。

use std::sync::Mutex;

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

static MARKS: Mutex<Vec<(String, f64)>> = Mutex::new(Vec::new());
/// 印を付けたときの private の MB（起動の報告で、どこでメモリが増えたかを見る）
static MEM: Mutex<Vec<(String, f64)>> = Mutex::new(Vec::new());

fn ft(f: FILETIME) -> i64 {
    ((f.dwHighDateTime as i64) << 32) | f.dwLowDateTime as i64
}

/// プロセス生成からの経過 ms
pub fn since_process_start_ms() -> f64 {
    let (mut c, mut e, mut k, mut u) = Default::default();
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u).unwrap() };
    let now = unsafe { GetSystemTimePreciseAsFileTime() };
    (ft(now) - ft(c)) as f64 / 10_000.0
}

pub fn mark(label: impl Into<String>) {
    let t = since_process_start_ms();
    let thread = std::thread::current().name().unwrap_or("?").to_string();
    let label = format!("[{thread}] {}", label.into());
    MEM.lock().unwrap().push((label.clone(), memory_mb().0));
    MARKS.lock().unwrap().push((label, t));
}

/// 同じラベルは最初の 1 回だけ記録する
pub fn mark_once(label: &str) {
    let tagged = format!("] {label}");
    if MARKS.lock().unwrap().iter().any(|(l, _)| l.ends_with(&tagged)) {
        return;
    }
    mark(label);
}

/// 記録した時刻（プロセス生成からの ms）
pub fn at(label: &str) -> Option<f64> {
    let tagged = format!("] {label}");
    MARKS.lock().unwrap().iter().find(|(l, _)| l.ends_with(&tagged)).map(|(_, t)| *t)
}

/// (private, working set) の MB
pub fn memory_mb() -> (f64, f64) {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
    let mut m = PROCESS_MEMORY_COUNTERS_EX { cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
    unsafe {
        let _ = GetProcessMemoryInfo(GetCurrentProcess(), &mut m as *mut _ as *mut PROCESS_MEMORY_COUNTERS, m.cb);
    }
    (m.PrivateUsage as f64 / 1048576.0, m.WorkingSetSize as f64 / 1048576.0)
}

pub fn report() {
    let mut marks = MARKS.lock().unwrap().clone();
    marks.sort_by(|a, b| a.1.total_cmp(&b.1));
    eprintln!("--- 起動（プロセス生成からの ms）---");
    let mem = MEM.lock().unwrap().clone();
    for (l, t) in &marks {
        let m = mem.iter().find(|(k, _)| k == l).map_or(String::new(), |(_, m)| format!("{m:>7.1}MB"));
        eprintln!("{t:>9.2} {m}  {l}");
    }
    let (private, ws) = memory_mb();
    eprintln!("memory: private={private:.1}MB working_set={ws:.1}MB");
}

