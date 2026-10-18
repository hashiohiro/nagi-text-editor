//! Windows の標準のダイアログ（フォルダの選択）。裏のスレッドで出し、その間も Nagi の画面は描き続ける。

use std::path::PathBuf;
use std::sync::OnceLock;

/// Nagi の窓（ダイアログの持ち主にする。main が窓を作ったときに入れる）
pub static MAIN_HWND: OnceLock<isize> = OnceLock::new();

/// フォルダを選ぶダイアログを出す。選んだら Some(パス)、やめたら None を送る。`start` はダイアログの最初のフォルダ
pub fn pick_folder(title: &str, start: Option<PathBuf>, waker: Option<nagi_workers::Waker>) -> nagi_core::task::Task<Option<PathBuf>> {
    let title = title.to_string();
    let rx = nagi_core::task::spawn("folder-dialog", waker.unwrap_or_else(nagi_core::task::no_waker), move |ctx| {
        let r = show(&title, start.as_deref());
        ctx.send(r);
    });
    rx
}

#[cfg(windows)]
fn show(title: &str, start: Option<&std::path::Path>) -> Option<PathBuf> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize};
    use windows::Win32::UI::Shell::{FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH};
    use windows::core::HSTRING;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let result = (|| -> Option<PathBuf> {
            let d: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
            let opts = d.GetOptions().ok()?;
            d.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
            let _ = d.SetTitle(&HSTRING::from(title));
            if let Some(s) = start {
                if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(s.as_os_str()), None) {
                    let _ = d.SetFolder(&item);
                }
            }
            let owner = MAIN_HWND.get().map(|h| HWND(*h as *mut _));
            d.Show(owner).ok()?;
            let item = d.GetResult().ok()?;
            let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            let s = name.to_string().ok();
            CoTaskMemFree(Some(name.0 as *const _));
            s.map(PathBuf::from)
        })();
        CoUninitialize();
        result
    }
}

#[cfg(not(windows))]
fn show(_title: &str, _start: Option<&std::path::Path>) -> Option<PathBuf> {
    None
}
