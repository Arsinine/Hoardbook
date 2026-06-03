//! Platform-specific private key encryption.
//!
//! Windows: CryptProtectData / CryptUnprotectData (DPAPI, user-scope).
//! Other:   Returns errors — callers gate on `cfg(target_os = "windows")`.

use anyhow::Result;

/// Encrypt `data` with the current Windows user's DPAPI key.
/// Returns the opaque ciphertext blob to persist on disk.
#[cfg(target_os = "windows")]
pub fn encrypt(data: &[u8]) -> Result<Vec<u8>> {
    win::encrypt(data)
}

/// Decrypt a blob previously produced by `encrypt`.
#[cfg(target_os = "windows")]
pub fn decrypt(data: &[u8]) -> Result<Vec<u8>> {
    win::decrypt(data)
}

#[cfg(not(target_os = "windows"))]
pub fn encrypt(_data: &[u8]) -> Result<Vec<u8>> {
    anyhow::bail!("DPAPI is only available on Windows")
}

#[cfg(not(target_os = "windows"))]
pub fn decrypt(_data: &[u8]) -> Result<Vec<u8>> {
    anyhow::bail!("DPAPI is only available on Windows")
}

// ---------------------------------------------------------------------------
// Windows implementation
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
mod win {
    use anyhow::{anyhow, Result};
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree, HLOCAL};
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData,
    };

    // ABI-equivalent to DATA_BLOB / CRYPT_INTEGER_BLOB: { DWORD cbData; BYTE *pbData; }
    #[repr(C)]
    struct DataBlob {
        cb: u32,
        pb: *mut u8,
    }

    pub fn encrypt(data: &[u8]) -> Result<Vec<u8>> {
        let src = DataBlob { cb: data.len() as u32, pb: data.as_ptr() as *mut u8 };
        let mut dst = DataBlob { cb: 0, pb: std::ptr::null_mut() };

        // CRYPTPROTECT_UI_FORBIDDEN = 0x8 — never show a GUI dialog.
        let ok = unsafe {
            CryptProtectData(
                &src as *const DataBlob as *const _,
                std::ptr::null(),        // no description string
                std::ptr::null(),        // no optional entropy
                std::ptr::null_mut(),    // pvReserved
                std::ptr::null(),        // no prompt
                0x8,
                &mut dst as *mut DataBlob as *mut _,
            )
        };

        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(anyhow!("CryptProtectData failed (Windows error {err:#010x})"));
        }

        let out = unsafe { std::slice::from_raw_parts(dst.pb, dst.cb as usize).to_vec() };
        unsafe { LocalFree(dst.pb as HLOCAL) };
        Ok(out)
    }

    pub fn decrypt(data: &[u8]) -> Result<Vec<u8>> {
        let src = DataBlob { cb: data.len() as u32, pb: data.as_ptr() as *mut u8 };
        let mut dst = DataBlob { cb: 0, pb: std::ptr::null_mut() };

        let ok = unsafe {
            CryptUnprotectData(
                &src as *const DataBlob as *const _,
                std::ptr::null_mut(),    // ppszDataDescr (ignored)
                std::ptr::null(),        // no optional entropy
                std::ptr::null_mut(),    // pvReserved
                std::ptr::null(),        // no prompt
                0,
                &mut dst as *mut DataBlob as *mut _,
            )
        };

        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(anyhow!("CryptUnprotectData failed (Windows error {err:#010x})"));
        }

        let out = unsafe { std::slice::from_raw_parts(dst.pb, dst.cb as usize).to_vec() };
        unsafe { LocalFree(dst.pb as HLOCAL) };
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    fn dpapi_encrypt_decrypt_roundtrip() {
        let plaintext = b"hb1_test_private_key_hex_here_1234567890abcdef";
        let ciphertext = encrypt(plaintext).unwrap();
        assert_ne!(ciphertext, plaintext, "ciphertext must differ from plaintext");
        let recovered = decrypt(&ciphertext).unwrap();
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn dpapi_tampered_ciphertext_fails() {
        let plaintext = b"secret";
        let mut ciphertext = encrypt(plaintext).unwrap();
        // Flip a byte in the middle.
        let mid = ciphertext.len() / 2;
        ciphertext[mid] ^= 0xff;
        assert!(decrypt(&ciphertext).is_err(), "tampered ciphertext must not decrypt");
    }
}
