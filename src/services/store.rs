//! Files under %LOCALAPPDATA%\FathNotch, the same folder (and file formats) as the earlier .NET build,
//! so to-dos, notes, settings and the TickTick token carry straight over. FATHNOTCH_DATA_DIR overrides it
//! (tests use a scratch folder).

use std::path::PathBuf;

use serde::{de::DeserializeOwned, Serialize};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

pub fn root() -> PathBuf {
    if let Ok(dir) = std::env::var("FATHNOTCH_DATA_DIR") {
        if !dir.is_empty() {
            let p = PathBuf::from(dir);
            let _ = std::fs::create_dir_all(&p);
            return p;
        }
    }
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    let p = PathBuf::from(base).join("FathNotch");
    let _ = std::fs::create_dir_all(&p);
    p
}

pub fn path(name: &str) -> PathBuf {
    root().join(name)
}

/// Reads a JSON file, or the default when it's missing or unreadable.
pub fn load<T: DeserializeOwned + Default>(name: &str) -> T {
    std::fs::read(path(name))
        .ok()
        .and_then(|b| {
            // .NET writes a UTF-8 BOM sometimes; serde_json doesn't accept it.
            let b = b.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(&b);
            serde_json::from_slice(b).ok()
        })
        .unwrap_or_default()
}

/// Writes JSON atomically (temp file, then rename).
pub fn save<T: Serialize>(name: &str, value: &T) {
    let p = path(name);
    let tmp = p.with_extension("tmp");
    if let Ok(text) = serde_json::to_string_pretty(value) {
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
}

/// DPAPI (current user) encryption, compatible with .NET's ProtectedData.
pub fn protect(plain: &[u8], entropy: &[u8]) -> Option<Vec<u8>> {
    unsafe {
        let input = blob(plain);
        let ent = blob(entropy);
        let mut out = CRYPT_INTEGER_BLOB::default();
        CryptProtectData(&input, None, Some(&ent), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out).ok()?;
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(out.pbData as _));
        Some(v)
    }
}

pub fn unprotect(cipher: &[u8], entropy: &[u8]) -> Option<Vec<u8>> {
    unsafe {
        let input = blob(cipher);
        let ent = blob(entropy);
        let mut out = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(&input, None, Some(&ent), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out).ok()?;
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(out.pbData as _));
        Some(v)
    }
}
