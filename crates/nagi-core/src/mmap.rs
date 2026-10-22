//! ファイルを読み取り専用でメモリに写す（フォント・型の索引）。
//!
//! `std::fs::read` で読むと中身がそのままプロセスの private になる（日本語フォントの TTC だけで 5 MB、窓を開くたびに増えた）。
//! 写したページは OS のファイルキャッシュと共有なので private に数えられず、OS がいつでも捨てて読み直せる。

use std::path::Path;
use std::sync::Arc;

use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, GetFileSizeEx, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, OPEN_EXISTING};
use windows_sys::Win32::System::Memory::{CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READONLY};

/// 写したファイルの中身（写せなければ読み込んだもの）
pub struct Mapped {
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
    owned: Vec<u8>,
}

// 読み取り専用の写しなので、どのスレッドから読んでもよい
unsafe impl Send for Mapped {}
unsafe impl Sync for Mapped {}

impl Mapped {
    /// `path` を写す。写せなければ読み込む
    pub fn open(path: &Path) -> Option<Arc<Mapped>> {
        match map(path) {
            Some((view, len)) => Some(Arc::new(Mapped { view, len, owned: Vec::new() })),
            None => Some(Arc::new(Mapped::from_vec(std::fs::read(path).ok()?))),
        }
    }

    pub fn from_vec(owned: Vec<u8>) -> Mapped {
        Mapped { view: MEMORY_MAPPED_VIEW_ADDRESS { Value: std::ptr::null_mut() }, len: 0, owned }
    }
}

fn map(path: &Path) -> Option<(MEMORY_MAPPED_VIEW_ADDRESS, usize)> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    unsafe {
        let file = CreateFileW(wide.as_ptr(), GENERIC_READ, FILE_SHARE_READ, std::ptr::null(), OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, std::ptr::null_mut());
        if file == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut size = 0i64;
        let ok = GetFileSizeEx(file, &mut size) != 0 && size > 0;
        let mapping = if ok { CreateFileMappingW(file, std::ptr::null(), PAGE_READONLY, 0, 0, std::ptr::null()) } else { std::ptr::null_mut() };
        CloseHandle(file);
        if mapping.is_null() {
            return None;
        }
        // 写しは閉じたハンドルに頼らない（ビューが残っている間は中身が保たれる）
        let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0);
        CloseHandle(mapping);
        (!view.Value.is_null()).then_some((view, size as usize))
    }
}

impl std::ops::Deref for Mapped {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        if self.view.Value.is_null() {
            &self.owned
        } else {
            unsafe { std::slice::from_raw_parts(self.view.Value as *const u8, self.len) }
        }
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        if !self.view.Value.is_null() {
            unsafe { UnmapViewOfFile(self.view) };
        }
    }
}
