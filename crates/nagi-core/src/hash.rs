//! SHA-256（Windows の CNG。文法の DLL の照合と、Salesforce の PKCE で使う）

/// SHA-256
#[cfg(windows)]
pub fn sha256(data: &[u8]) -> crate::Result<[u8; 32]> {
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
    let mut out = [0u8; 32];
    // SAFETY: 入力と出力の長さをそのまま渡す
    let st = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, std::ptr::null(), 0, data.as_ptr(), data.len() as u32, out.as_mut_ptr(), 32) };
    if st != 0 {
        return Err(format!("SHA-256 を計算できません（{st:#x}）").into());
    }
    Ok(out)
}

#[cfg(not(windows))]
pub fn sha256(_: &[u8]) -> crate::Result<[u8; 32]> {
    Err("この OS では使えません".into())
}

/// 16 進の SHA-256
pub fn sha256_hex(data: &[u8]) -> Option<String> {
    Some(sha256(data).ok()?.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn known_value() {
        assert_eq!(super::sha256_hex(b"abc").unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
