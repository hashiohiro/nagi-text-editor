//! タスクバーのジャンプリスト（nagi のボタンを右クリックしたときの一覧）。
//!
//! 「ピン留め」と「最近のプロジェクト」を載せる。選ぶと `nagi.exe "<フォルダ>"` で起動し、そのプロジェクトを開く。
//! 窓とジャンプリストを同じ nagi のものとして扱わせるため、起動の初めに AppUserModelID を決める。

use std::path::PathBuf;

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Storage::EnhancedStorage::PKEY_Title;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemAlloc, CoUninitialize};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW, SetCurrentProcessExplicitAppUserModelID, ShellLink};
use windows::core::{HSTRING, Interface, PWSTR};

const APP_ID: &str = "Nagi.Editor";

/// 起動の初め（窓を作る前）に呼ぶ
pub fn set_app_id() {
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(APP_ID));
    }
}

/// ジャンプリストを今の記録で作り直す（裏のスレッドで。試験では何もしない）
pub fn refresh() {
    if cfg!(test) {
        return;
    }
    nagi_core::task::detach("jumplist", || {
        let entries = crate::project_open::recent_entries();
        let pinned: Vec<(String, PathBuf)> = entries.iter().filter(|e| e.pinned && !e.missing).map(|e| (e.name.clone(), e.path.clone())).collect();
        let n = crate::settings::Settings::load(None).int("other.jumplist_count") as usize;
        let recent: Vec<(String, PathBuf)> = entries.iter().filter(|e| !e.pinned && !e.missing).take(n).map(|e| (e.name.clone(), e.path.clone())).collect();
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            if let Err(e) = build(&pinned, &recent) {
                nagi_core::log::warn(&format!("ジャンプリストを作れない: {e}"));
            }
            CoUninitialize();
        }
    });
}

unsafe fn build(pinned: &[(String, PathBuf)], recent: &[(String, PathBuf)]) -> windows::core::Result<()> {
    unsafe {
        let exe = std::env::current_exe().map_err(|_| windows::core::Error::empty())?;
        let list: ICustomDestinationList = CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER)?;
        list.SetAppID(&HSTRING::from(APP_ID))?;
        let mut slots = 0u32;
        let _removed: IObjectArray = list.BeginList(&mut slots)?;
        for (cat, items) in [("ピン留め", pinned), ("最近のプロジェクト", recent)] {
            if items.is_empty() {
                continue;
            }
            let col: IObjectCollection = CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER)?;
            for (name, path) in items {
                col.AddObject(&link(&exe, name, path)?)?;
            }
            let arr: IObjectArray = col.cast()?;
            list.AppendCategory(&HSTRING::from(cat), &arr)?;
        }
        list.CommitList()
    }
}

/// `nagi.exe "<フォルダ>"` のショートカット（表示はプロジェクトの名前、説明はフォルダ）
unsafe fn link(exe: &std::path::Path, name: &str, path: &std::path::Path) -> windows::core::Result<IShellLinkW> {
    unsafe {
        let l: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        l.SetPath(&HSTRING::from(exe.as_os_str()))?;
        l.SetArguments(&HSTRING::from(format!("\"{}\"", path.display())))?;
        l.SetDescription(&HSTRING::from(path.as_os_str()))?;
        l.SetIconLocation(&HSTRING::from(exe.as_os_str()), 0)?;
        let store: IPropertyStore = l.cast()?;
        set_string(&store, &PKEY_Title, name)?;
        store.Commit()?;
        Ok(l)
    }
}

/// 文字列（VT_LPWSTR）の値を入れる
unsafe fn set_string(store: &IPropertyStore, key: &PROPERTYKEY, s: &str) -> windows::core::Result<()> {
    unsafe {
        let w: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
        let p = CoTaskMemAlloc(w.len() * 2) as *mut u16;
        if p.is_null() {
            return Err(windows::core::Error::empty());
        }
        std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
        let mut v = PROPVARIANT::default();
        (*v.Anonymous.Anonymous).vt = VT_LPWSTR;
        (*v.Anonymous.Anonymous).Anonymous.pwszVal = PWSTR(p);
        let r = store.SetValue(key, &v);
        let _ = PropVariantClear(&mut v);
        r
    }
}
