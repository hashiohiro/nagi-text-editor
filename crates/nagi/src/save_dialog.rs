//! 「名前を付けて保存」のダイアログ（Windows の IFileSaveDialog）。
//!
//! UI スレッドを止めないよう、別のスレッドで COM を初めてから出す。選んだパスはチャネルで返し、UI を起こす。
//! 上書きの確認はダイアログ自身が出す（FOS_OVERWRITEPROMPT）。

use std::path::PathBuf;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::UI::Shell::{FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FileSaveDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH};
use windows::core::HSTRING;

fn pick(owner: isize, dir: Option<PathBuf>, name: Option<String>) -> Option<PathBuf> {
    unsafe {
        let d: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = d.GetOptions().ok()?;
        let _ = d.SetOptions(opts | FOS_OVERWRITEPROMPT | FOS_FORCEFILESYSTEM);
        let _ = d.SetTitle(&HSTRING::from("名前を付けて保存"));
        if let Some(n) = name {
            let _ = d.SetFileName(&HSTRING::from(n));
        }
        if let Some(dir) = dir {
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(dir.as_os_str()), None) {
                let _ = d.SetFolder(&item);
            }
        }
        // 取り消したときは Err（ERROR_CANCELLED）
        d.Show(Some(HWND(owner as *mut _))).ok()?;
        let item = d.GetResult().ok()?;
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

/// ダイアログを出す。選んだパス（取り消したら None）が届いたら `waker` で UI を起こす
pub fn open(owner: isize, dir: Option<PathBuf>, name: Option<String>, waker: Option<std::sync::Arc<dyn Fn() + Send + Sync>>) -> nagi_core::task::Task<Option<PathBuf>> {
    nagi_core::task::spawn("save-dialog", waker.unwrap_or_else(nagi_core::task::no_waker), move |ctx| {
        let ok = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok() };
        let r = pick(owner, dir, name);
        if ok {
            unsafe { CoUninitialize() };
        }
        ctx.send(r);
    })
}
