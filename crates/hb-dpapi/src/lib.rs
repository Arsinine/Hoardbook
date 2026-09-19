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

/// Restrict `path` to an owner-only DACL (QURATOR-234).
///
/// `hb-app` carries `#![forbid(unsafe_code)]`, which is why this crate exists: it is the one place
/// Windows FFI lives. The plaintext identity export (nsec + browse-key) must not inherit its parent
/// directory's typically-broad ACL, and there are no Unix mode bits to fall back on.
///
/// **Call this on an EMPTY file, before the secret bytes are written** — the same ordering the Unix
/// arm gets from `.mode(0o600)`-on-open. Applying it after writing leaves a window in which the
/// plaintext key sits on disk world-readable, which is the exposure this exists to close.
///
/// **What it protects against:** another local, non-elevated account reading the export via the
/// filesystem — the DACL grants access to the owner alone.
/// **What it does NOT:** an administrator or SYSTEM (which can take ownership), physical/full-disk
/// access, malware running as the same user, or backup/shadow-copy paths that read beneath the ACL
/// layer. It is not a substitute for encrypting the export.
#[cfg(target_os = "windows")]
pub fn restrict_to_owner(path: &str) -> Result<()> {
    win::restrict_to_owner(path)
}

/// Non-Windows: there is nothing to do — callers use Unix mode bits instead.
#[cfg(not(target_os = "windows"))]
pub fn restrict_to_owner(_path: &str) -> Result<()> {
    Ok(())
}

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

        // CRYPTPROTECT_UI_FORBIDDEN = 0x1 — never show a GUI dialog.
        // NOTE: 0x8 is CRYPTPROTECT_CRED_SYNC, which performs a credential-sync
        // operation and returns TRUE *without encrypting*, leaving pDataOut null.
        let ok = unsafe {
            CryptProtectData(
                &src as *const DataBlob as *const _,
                std::ptr::null(),        // no description string
                std::ptr::null(),        // no optional entropy
                std::ptr::null_mut(),    // pvReserved
                std::ptr::null(),        // no prompt
                0x1,
                &mut dst as *mut DataBlob as *mut _,
            )
        };

        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(anyhow!("CryptProtectData failed (Windows error {err:#010x})"));
        }
        if dst.pb.is_null() {
            return Err(anyhow!("CryptProtectData reported success but returned a null blob"));
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
        if dst.pb.is_null() {
            return Err(anyhow!("CryptUnprotectData reported success but returned a null blob"));
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

    // HANDOVER scenario 1: the `0x8` (CRED_SYNC) flag returned TRUE without
    // encrypting and left pDataOut null, so `encrypt` produced an EMPTY blob.
    // The roundtrip test above could not catch that on its own (empty != plaintext
    // still holds), so assert the ciphertext is actually present.
    #[test]
    fn dpapi_ciphertext_is_nonempty_and_differs() {
        let plaintext = b"hb1_test_private_key_hex_here_1234567890abcdef";
        let ciphertext = encrypt(plaintext).unwrap();
        assert!(
            !ciphertext.is_empty(),
            "DPAPI ciphertext must be non-empty (CRED_SYNC 0x8 returned an empty blob)"
        );
        assert_ne!(
            ciphertext.as_slice(),
            plaintext.as_slice(),
            "ciphertext must differ from plaintext"
        );
    }

    // HANDOVER scenario 1: a 0-byte keypair.bin (the symptom on disk) must fail
    // cleanly, never panic. decrypt(&[]) exercises the empty-input path.
    #[test]
    fn dpapi_decrypt_rejects_empty_blob() {
        assert!(
            decrypt(&[]).is_err(),
            "decrypting an empty blob must return Err, not panic or succeed"
        );
    }

    // HANDOVER scenario 1: arbitrary non-DPAPI bytes must be rejected, never UB.
    #[test]
    fn dpapi_decrypt_rejects_garbage() {
        assert!(
            decrypt(b"\x00\x01\x02").is_err(),
            "decrypting non-DPAPI garbage must return Err, not panic"
        );
    }

    /// Owner-only, protected DACL via `SetNamedSecurityInfoW`. "D:" opens a DACL, "P" marks it
    /// protected (so inherited ACEs cannot re-add broader access), and the single ACE grants Full
    /// Access to the owner ("OW" = Owner Rights, S-1-3-4).
    pub fn restrict_to_owner(path: &str) -> Result<()> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SetNamedSecurityInfoW,
            SDDL_REVISION_1, SE_FILE_OBJECT,
        };
        use windows_sys::Win32::Security::{
            GetSecurityDescriptorDacl, ACL, DACL_SECURITY_INFORMATION,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        };
        use windows_sys::Win32::Foundation::ERROR_SUCCESS;

        const OWNER_ONLY_SDDL: &str = "D:P(A;;FA;;;OW)\0";
        let sddl_wide: Vec<u16> = OWNER_ONLY_SDDL.encode_utf16().collect();
        let path_wide: Vec<u16> = {
            let mut v: Vec<u16> = std::ffi::OsStr::new(path).encode_wide().collect();
            v.push(0);
            v
        };

        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let mut sd_size: u32 = 0;
        // SAFETY: `sddl_wide` is a NUL-terminated UTF-16 string alive for the call; `sd`/`sd_size`
        // are out-params written only on a nonzero return.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl_wide.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                &mut sd_size,
            )
        };
        if ok == 0 {
            return Err(anyhow!("building the owner-only security descriptor failed"));
        }

        let mut dacl_present = 0;
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut dacl_defaulted = 0;
        // SAFETY: `sd` was just produced above and is freed via `LocalFree` on every path out.
        let got = unsafe {
            GetSecurityDescriptorDacl(sd, &mut dacl_present, &mut dacl, &mut dacl_defaulted)
        };
        if got == 0 {
            unsafe { LocalFree(sd as HLOCAL) };
            return Err(anyhow!("reading the DACL out of the security descriptor failed"));
        }

        // SAFETY: `path_wide` is NUL-terminated and outlives the call; `dacl` points into `sd`,
        // still alive here. Null owner/group/sacl leave those unchanged.
        let set = unsafe {
            SetNamedSecurityInfoW(
                path_wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl as *const ACL,
                std::ptr::null(),
            )
        };
        unsafe { LocalFree(sd as HLOCAL) };
        if set != ERROR_SUCCESS {
            return Err(anyhow!("applying the owner-only DACL failed (win32 error {set})"));
        }
        Ok(())
    }
}

