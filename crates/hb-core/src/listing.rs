//! Collection-listing encryption — NIP-44 v2 under a *symmetric* browse-key
//! (spec §The Collection, §Data Model).
//!
//! NIP-44's headline API is ECDH between two secp256k1 keys, but the browse-key is a shared
//! 32-byte symmetric secret. `nostr`'s `nip44::v2::ConversationKey::new([u8; 32])` accepts a
//! raw conversation key, which we derive from the browse-key through a **versioned HKDF** —
//! the crypto-version byte is the HKDF `info`, so each version is a domain-separated key. The
//! version is carried in the listing event's **signed tag** and checked on decrypt, so an
//! unknown version is *recognised and refused*, never mis-decrypted. The ciphertext is
//! **base64** (the NIP-44 standard content encoding), ready to drop into a Nostr event.
//!
//! ## zstd before the seal (QURATOR-344 slice B)
//!
//! The body is compressed **before** it is ever a ciphertext: `encrypt_listing` frames
//! `[LISTING_PAYLOAD_V] ++ zstd(json)` and seals that, so the relay-hosted bytes are the
//! compressed shape (measured: 10k-file deep chain 948 KB → 9 KB; 2000-film library 732 KB →
//! 23 KB). Decrypting inverts it under a **hard decompressed-byte cap**
//! ([`MAX_DECOMPRESSED_LISTING_BYTES`]) — a `Read::take(cap + 1)` bound refuses a crafted
//! high-ratio frame (a "zip bomb") at the ceiling instead of materializing it. The frame byte is
//! the payload discriminant living INSIDE the signed ciphertext: it moves without touching
//! `CRYPTO_V`/`SCHEMA_V` (whose bumps re-key the share-code framing, the DM cache, and every
//! other kind), and the pre-bump v1 body (raw JSON, no frame byte) is *recognised and refused*,
//! never dual-read — there were no readers in the wild when it shipped.

use std::io::{Cursor, Read};

use base64::Engine as _;
use hkdf::Hkdf;
use nostr::nips::nip44::v2::{decrypt_to_bytes, encrypt_to_bytes, ConversationKey};
use sha2::Sha256;

use crate::error::HbError;
use crate::version::{check_crypto, CRYPTO_V};

/// A 32-byte symmetric browse-key.
pub type BrowseKey = [u8; 32];

/// A 32-byte random **content-encryption key** (CEK). A private listing's body is sealed once
/// under a fresh CEK (spec §Private Collections; M10 Decision A), and that CEK is then wrapped to
/// each trusted `npub`. Distinct type-alias from [`BrowseKey`] for readability — the two derive
/// **domain-separated** NIP-44 keys (different HKDF salt + info), so the browse-key can never open
/// a CEK-sealed body even if the byte values coincided.
pub type ContentKey = [u8; 32];

pub(crate) const HKDF_SALT: &[u8] = b"hoardbook/browse-key";
pub(crate) const HKDF_SALT_CEK: &[u8] = b"hoardbook/cek";
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// Wire-format version of the listing/manifest **body payload** — the first plaintext byte
/// inside the NIP-44 envelope, after the HKDF→NIP-44 layer (value pinned in `wire_freeze`).
/// **v2** seals `zstd(json)` behind the frame byte; **v1** was the pre-QURATOR-344 raw-JSON
/// body, which carried no framing byte at all (every v1 body begins `{`) and is refused on
/// decrypt.
pub const LISTING_PAYLOAD_V: u8 = 2;

/// Hard ceiling on the **decompressed** listing/manifest body (QURATOR-344 slice B, the
/// zip-bomb guard). Derivation: the largest body the app can legitimately build is
/// `MAX_COLLECTION_ITEMS` = 100 000 items (hb-app `scan_selective`) at an upper bound of ~640
/// bytes/item (255-char description + path + name + tags), ≈ 64 MiB — every legitimate body
/// fits, so anything larger is a hostile or broken peer. Frozen in `wire_freeze` like the
/// transport ceiling: two peers that disagree about it would disagree about whether a transfer
/// is refused or runs away.
pub const MAX_DECOMPRESSED_LISTING_BYTES: u64 = 64 * 1024 * 1024;

/// Derive the NIP-44 conversation key from the browse-key for a given crypto version.
/// The HKDF `info` is a labelled, version-bearing context string (RFC 5869 domain
/// separation, matching the labelled convention in `crypto.rs`), so each crypto version
/// derives an independent key.
fn conversation_key(browse_key: &BrowseKey, crypto_v: u8) -> ConversationKey {
    let mut info = b"hoardbook/browse-key/v".to_vec();
    info.push(crypto_v);
    let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), browse_key);
    let mut ck = [0u8; 32];
    hk.expand(&info, &mut ck)
        .expect("32 is a valid HKDF-SHA256 output length");
    ConversationKey::new(ck)
}

/// Frame + compress the listing body: `[LISTING_PAYLOAD_V] ++ zstd(plain)`. The frame byte is the
/// payload version INSIDE the envelope — it discriminates the sealed bytes' meaning without
/// touching `CRYPTO_V` (a bump there re-keys the share-code framing and the on-disk DM cache).
/// Level 0 = zstd's own default.
fn compress_body(plain: &[u8]) -> Result<Vec<u8>, HbError> {
    let compressed =
        zstd::stream::encode_all(plain, 0).map_err(|_| HbError::EncryptionFailed)?;
    let mut framed = Vec::with_capacity(compressed.len() + 1);
    framed.push(LISTING_PAYLOAD_V);
    framed.extend_from_slice(&compressed);
    Ok(framed)
}

/// Inverse of [`compress_body`]: check the frame byte, then decompress under the hard
/// [`MAX_DECOMPRESSED_LISTING_BYTES`] ceiling — `Read::take(cap + 1)` stops the decoder at the
/// cap, so a bomb is refused without ever materializing its full output. An unknown frame byte
/// (including the v1 raw-JSON body, which begins `{`) is [`HbError::UnsupportedVersion`] carrying
/// that byte — recognised and refused after the NIP-44 MAC, never mis-decoded, never dual-read. A
/// DISTINCT error from a corrupt zstd stream's `InvalidEncryptedMessage`, so the version refusal is
/// observable on its own (without it, raw JSON reaching the decoder fails anyway and hides a
/// deleted version check).
fn decompress_body(framed: &[u8]) -> Result<Vec<u8>, HbError> {
    match framed.first() {
        Some(&LISTING_PAYLOAD_V) => {}
        Some(&other) => return Err(HbError::UnsupportedVersion(other)),
        None => return Err(HbError::InvalidEncryptedMessage),
    }
    let decoder = zstd::stream::Decoder::new(Cursor::new(&framed[1..]))
        .map_err(|_| HbError::InvalidEncryptedMessage)?;
    let mut capped = decoder.take(MAX_DECOMPRESSED_LISTING_BYTES + 1);
    let mut out = Vec::new();
    capped
        .read_to_end(&mut out)
        .map_err(|_| HbError::InvalidEncryptedMessage)?;
    if out.len() as u64 > MAX_DECOMPRESSED_LISTING_BYTES {
        return Err(HbError::InvalidManifest(format!(
            "listing body decompressed past the {}-byte decompression cap ({} bytes and counting) — refused as a hostile or broken peer",
            MAX_DECOMPRESSED_LISTING_BYTES, out.len()
        )));
    }
    Ok(out)
}

/// Encrypt a listing under the browse-key at the current crypto version. The body is
/// zstd-compressed and framed with [`LISTING_PAYLOAD_V`] *before* the seal ([`compress_body`]),
/// so the relay-hosted ciphertext is the compressed shape. Returns the base64 content for the
/// listing event; the caller records [`CRYPTO_V`] in the event's signed tag.
pub fn encrypt_listing(browse_key: &BrowseKey, listing_json: &str) -> Result<String, HbError> {
    let ck = conversation_key(browse_key, CRYPTO_V);
    let framed = compress_body(listing_json.as_bytes())?;
    let bytes = encrypt_to_bytes(&ck, &framed).map_err(|e| HbError::Nostr(e.to_string()))?;
    Ok(B64.encode(bytes))
}

/// The sealed length of `framed` bytes under NIP-44 v2 **plus base64**, exactly as
/// [`encrypt_listing`] produces it: `1` version byte `+ 32` nonce `+ (2 + padding)` length-prefixed
/// padded plaintext `+ 32` HMAC, base64'd at 4 chars per 3 bytes. The padding schedule
/// ([`nip44_calc_padding`]) is nostr's, reproduced verbatim so the count is exact.
const fn sealed_bytes_len(framed_len: usize) -> usize {
    let padded = nip44_calc_padding(framed_len);
    let payload = 1 + 32 + (2 + padded) + 32;
    payload.div_ceil(3) * 4
}

/// NIP-44 v2's padding schedule (nostr 0.44 `calc_padding`, reproduced verbatim): ≤32 rounds up to
/// 32; above that the next power of two sets a chunk size (`32` up to 256, else `power/8`) and the
/// length rounds up to the next multiple of that chunk. Reproduced here — not called — because the
/// padded length is a pure function of plaintext length and the budget checks must not pay for a
/// key, a nonce, or a compression of anything but the body being measured.
const fn nip44_calc_padding(len: usize) -> usize {
    if len <= 32 {
        return 32;
    }
    let log2_floor = (usize::BITS - 1) - (len - 1).leading_zeros();
    let nextpower = 1usize << (log2_floor + 1);
    let chunk = if nextpower <= 256 { 32 } else { nextpower / 8 };
    chunk * (((len - 1) / chunk) + 1)
}

/// NIP-44 v2's largest sealable plaintext (nostr `MAX_SUPPORTED_PLAINTEXT_SIZE` = 65_536 − 128).
pub const MAX_SEAL_PLAINTEXT: usize = 65_408;

/// The byte length of the base64 event content [`encrypt_listing`] would produce for
/// `listing_json` — the SEALED size, what the relay actually stores and serves. NIP-44 v2's
/// ciphertext length is deterministic in the plaintext length (the nonce is a fixed 32 bytes in the
/// payload; only its content is random), so this is an **exact** measure, not an estimate. Cost: one
/// zstd default-level compression pass over the JSON, no key material, no nonce — cheap enough to
/// call per part during split/truncate budgeting.
///
/// Returns `usize::MAX` when the framed body would exceed [`MAX_SEAL_PLAINTEXT`] (or zstd fails) —
/// a length no listing can be sealed at, which every `<= budget` comparison reads as over-budget.
pub fn sealed_listing_len(listing_json: &str) -> usize {
    let compressed = match zstd::stream::encode_all(listing_json.as_bytes(), 0) {
        Ok(c) => c,
        Err(_) => return usize::MAX,
    };
    // compress_body frames `[LISTING_PAYLOAD_V] ++ zstd(plain)` — one byte before the stream.
    let framed_len = 1 + compressed.len();
    if framed_len > MAX_SEAL_PLAINTEXT {
        return usize::MAX;
    }
    sealed_bytes_len(framed_len)
}

/// Derive the NIP-44 conversation key from a **content-encryption key** for a given crypto
/// version. Same labelled-HKDF construction as [`conversation_key`] (RFC 5869 domain separation),
/// but with a **distinct salt** (`hoardbook/cek`) and `info` (`hoardbook/cek/v…`), so a CEK and a
/// browse-key with identical bytes still derive *different* keys — the browse-key path can never
/// open a CEK-sealed body (M10 Decision A', the headline negative).
fn cek_conversation_key(cek: &ContentKey, crypto_v: u8) -> ConversationKey {
    let mut info = b"hoardbook/cek/v".to_vec();
    info.push(crypto_v);
    let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT_CEK), cek);
    let mut ck = [0u8; 32];
    hk.expand(&info, &mut ck)
        .expect("32 is a valid HKDF-SHA256 output length");
    ConversationKey::new(ck)
}

/// Encrypt a private-listing **body** under a content-encryption key at the current crypto
/// version (M10). The CEK is a raw 32-byte symmetric key, so this is the same HKDF→NIP-44-v2
/// **symmetric** primitive the browse-key path ships — *not* a raw `NIP-44_encrypt(CEK,…)` call
/// (NIP-44's public API is ECDH-keyed; a CEK is not a secp256k1 private key). The caller records
/// [`CRYPTO_V`] in the wrap + the inner event's signed `hb-cv` tag.
pub fn encrypt_with_cek(cek: &ContentKey, plaintext: &str) -> Result<String, HbError> {
    let ck = cek_conversation_key(cek, CRYPTO_V);
    let bytes =
        encrypt_to_bytes(&ck, plaintext.as_bytes()).map_err(|e| HbError::Nostr(e.to_string()))?;
    Ok(B64.encode(bytes))
}

/// Decrypt a private-listing body. `crypto_v` is the version carried in the (recipient-decrypted)
/// CEK wrap + the inner event tag; an unknown version is refused before any decryption is
/// attempted (the same forward-compat contract `decrypt_listing` upholds).
pub fn decrypt_with_cek(
    cek: &ContentKey,
    crypto_v: u8,
    content_b64: &str,
) -> Result<String, HbError> {
    check_crypto(crypto_v)?;
    let ck = cek_conversation_key(cek, crypto_v);
    let bytes = B64
        .decode(content_b64.as_bytes())
        .map_err(|_| HbError::InvalidEncryptedMessage)?;
    let plain = decrypt_to_bytes(&ck, &bytes).map_err(|_| HbError::DecryptionFailed)?;
    String::from_utf8(plain).map_err(|_| HbError::DecryptionFailed)
}

/// Decrypt a listing. `crypto_v` is the version read from the listing event's signed tag; an
/// unknown version is refused before any decryption is attempted. Inside the envelope the body
/// carries its own [`LISTING_PAYLOAD_V`] frame byte — checked after the NIP-44 MAC, so an
/// authenticated payload in an unknown format is *recognised and refused*, never mis-decoded —
/// and must decompress within [`MAX_DECOMPRESSED_LISTING_BYTES`].
pub fn decrypt_listing(
    browse_key: &BrowseKey,
    crypto_v: u8,
    content_b64: &str,
) -> Result<String, HbError> {
    check_crypto(crypto_v)?;
    let ck = conversation_key(browse_key, crypto_v);
    let bytes = B64
        .decode(content_b64.as_bytes())
        .map_err(|_| HbError::InvalidEncryptedMessage)?;
    let plain = decrypt_to_bytes(&ck, &bytes).map_err(|_| HbError::DecryptionFailed)?;
    let json = decompress_body(&plain)?;
    String::from_utf8(json).map_err(|_| HbError::DecryptionFailed)
}

/// Test-only: seal a body the PRE-bump v1 way (raw JSON behind NIP-44, no frame byte), so the
/// old-format refusal stays exercisable now that v2 is the only producer shape.
#[cfg(test)]
pub(crate) fn seal_v1_raw(browse_key: &BrowseKey, listing_json: &str) -> String {
    let ck = conversation_key(browse_key, CRYPTO_V);
    let bytes = encrypt_to_bytes(&ck, listing_json.as_bytes()).expect("a v1 body seals");
    B64.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str =
        r#"{"slug":"criterion","content_types":["video"],"items":[{"name":"Seven Samurai"}]}"#;

    #[test]
    fn browse_key_roundtrip() {
        let bk: BrowseKey = rand::random();
        let ct = encrypt_listing(&bk, LISTING).unwrap();
        assert_eq!(decrypt_listing(&bk, CRYPTO_V, &ct).unwrap(), LISTING);
    }

    #[test]
    fn content_is_base64_nip44_not_hex() {
        let bk: BrowseKey = rand::random();
        let ct = encrypt_listing(&bk, LISTING).unwrap();
        // Valid base64 whose bytes are a NIP-44 v2 payload (version byte 0x02 first).
        let raw = B64.decode(ct.as_bytes()).expect("content must be base64");
        assert_eq!(raw[0], 2, "NIP-44 v2 payload begins with version byte 2");
    }

    #[test]
    fn sealed_len_matches_the_real_seal_at_every_bucket() {
        // The budget checks measure with `sealed_listing_len` and never with the plaintext, so the
        // formula must be byte-exact against the real seal — including at the padding bucket edges
        // (32 / 33 / 64 / 65 / 4096 / 4097) and the top of the sealable range (65,408, where the
        // payload is at MAX_PAYLOAD_SIZE). `incompressible` uses xorshift noise so the compressed
        // length tracks the requested plaintext length instead of collapsing.
        let bk: BrowseKey = [9u8; 32];
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut noise = |len: usize| -> String {
            (0..len)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    ALPHABET[(state & 63) as usize] as char
                })
                .collect()
        };
        let mut sizes: Vec<usize> = vec![0, 1, 31, 32, 33, 63, 64, 65, 100, 1000, 4096, 4097];
        sizes.extend([30_000, 65_407, 65_408]);
        for &size in &sizes {
            let json = if size == 0 {
                String::new()
            } else {
                noise(size)
            };
            assert_eq!(
                sealed_listing_len(&json),
                encrypt_listing(&bk, &json).unwrap().len(),
                "sealed length must be exact at plaintext size {size}"
            );
        }
        // Past the cap the seal would fail — the measure reads over-budget instead. The cap bites
        // on the COMPRESSED framing (1 frame byte + zstd stream), so plain 65,409 noise bytes
        // compress to ~57 KB and still seal — the fixture must be big enough that its compressed
        // size clears the cap too.
        let over = noise(100_000);
        // P-10 mutation: in `sealed_bytes_len`, change the trailing `+ 32` (HMAC) to `+ 31` —
        // must red this test (the measure under-reads every real seal by 1 base64 byte).
        assert_eq!(
            sealed_listing_len(&over),
            usize::MAX,
            "a body whose compressed framing exceeds the NIP-44 plaintext cap must measure unsealable"
        );
        // And MAX must mean exactly "the real seal refuses": pin the correspondence.
        assert!(
            encrypt_listing(&bk, &over).is_err(),
            "usize::MAX must correspond to a seal that actually fails"
        );
    }

    #[test]
    fn ciphertext_nonempty_and_ne_plaintext() {
        let bk: BrowseKey = rand::random();
        let ct = encrypt_listing(&bk, LISTING).unwrap();
        assert!(!ct.is_empty());
        assert_ne!(ct, LISTING);
    }

    #[test]
    fn wrong_browse_key_fails_cleanly() {
        let a: BrowseKey = rand::random();
        let b: BrowseKey = rand::random();
        let ct = encrypt_listing(&a, LISTING).unwrap();
        assert!(matches!(decrypt_listing(&b, CRYPTO_V, &ct), Err(HbError::DecryptionFailed)));
    }

    #[test]
    fn nonce_is_unique_per_encryption() {
        let bk: BrowseKey = rand::random();
        let a = encrypt_listing(&bk, LISTING).unwrap();
        let b = encrypt_listing(&bk, LISTING).unwrap();
        assert_ne!(a, b, "NIP-44 uses a random nonce; identical ciphertext would be a red flag");
    }

    #[test]
    fn unknown_kdf_version_is_recognised_not_misdecrypted() {
        // A signed tag claiming a future crypto version is refused cleanly, not decrypted
        // under a wrong key (which would surface as a confusing MAC failure).
        let bk: BrowseKey = rand::random();
        let ct = encrypt_listing(&bk, LISTING).unwrap();
        assert!(matches!(
            decrypt_listing(&bk, CRYPTO_V + 1, &ct),
            Err(HbError::UnsupportedVersion(v)) if v == CRYPTO_V + 1
        ));
    }

    #[test]
    fn cek_body_roundtrips() {
        let cek: ContentKey = rand::random();
        let ct = encrypt_with_cek(&cek, LISTING).unwrap();
        assert_eq!(decrypt_with_cek(&cek, CRYPTO_V, &ct).unwrap(), LISTING);
    }

    #[test]
    fn cek_wrong_key_fails_cleanly() {
        let a: ContentKey = rand::random();
        let b: ContentKey = rand::random();
        let ct = encrypt_with_cek(&a, LISTING).unwrap();
        assert!(matches!(decrypt_with_cek(&b, CRYPTO_V, &ct), Err(HbError::DecryptionFailed)));
    }

    #[test]
    fn cek_unknown_version_is_recognised_not_misdecrypted() {
        let cek: ContentKey = rand::random();
        let ct = encrypt_with_cek(&cek, LISTING).unwrap();
        assert!(matches!(
            decrypt_with_cek(&cek, CRYPTO_V + 1, &ct),
            Err(HbError::UnsupportedVersion(v)) if v == CRYPTO_V + 1
        ));
    }

    #[test]
    fn cek_and_browse_key_are_domain_separated() {
        // THE HEADLINE NEGATIVE (helper level): even if a CEK and a browse-key held the *same 32
        // bytes*, the CEK-keyed body cannot be opened by the browse-key path — the HKDF salt/info
        // differ, so the derived conversation keys differ. A browse-key can NEVER read a private
        // body. (The wire-level version of this lives in priv_listing's `open` negatives.)
        let shared: [u8; 32] = rand::random();
        let body = encrypt_with_cek(&shared, LISTING).unwrap();
        // Same bytes, but interpreted as a browse-key → must NOT decrypt the CEK-sealed body.
        assert!(
            decrypt_listing(&shared, CRYPTO_V, &body).is_err(),
            "a browse-key must not open a body sealed under the same bytes as a CEK"
        );
        // And the conversation keys are concretely different.
        let cek_ck = cek_conversation_key(&shared, CRYPTO_V);
        let bk_ck = conversation_key(&shared, CRYPTO_V);
        let probe = encrypt_to_bytes(&cek_ck, b"x").unwrap();
        assert!(decrypt_to_bytes(&bk_ck, &probe).is_err(), "CEK vs browse-key keys must diverge");
    }

    #[test]
    fn kdf_versions_are_domain_separated() {
        // The forward-compat seam: a future version derives a different conversation key, so
        // v1 ciphertext would not decrypt under a v2 key (tested at the helper level since the
        // public API only admits the current version today).
        let bk: BrowseKey = rand::random();
        let v1 = conversation_key(&bk, 1);
        let v2 = conversation_key(&bk, 2);
        let ct = encrypt_to_bytes(&v1, b"listing").unwrap();
        assert!(decrypt_to_bytes(&v2, &ct).is_err(), "a KDF version bump must domain-separate");
        assert_eq!(decrypt_to_bytes(&v1, &ct).unwrap(), b"listing");
    }

    // ---------- QURATOR-344 slice B: zstd before the seal ----------

    /// Craft a raw zstd frame that regenerates 513 RLE blocks of 131,071 bytes — 67,239,423
    /// bytes, ~130 KB over [`MAX_DECOMPRESSED_LISTING_BYTES`] (64 MiB) — from 2,058 bytes
    /// (~32,000:1). Hand-built from spec RLE blocks (magic, an FHD with no Frame_Content_Size,
    /// a 128 KiB window descriptor, then RLE block headers carrying Regenerated_Size directly)
    /// so the bomb is deterministic regardless of compressor versions AND fits the 65,408-byte
    /// NIP-44 plaintext ceiling: a conforming decoder streams it from a 128 KiB window, so the
    /// only thing that can stop it is OUR cap. (Frame validated against the reference zstd
    /// binary: decodes to exactly 67,239,423 bytes.)
    fn crafted_rle_bomb() -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&0xFD2FB528_u32.to_le_bytes()); // magic
        f.push(0x00); // FHD: no FCS, not single-segment, no checksum, no dict
        f.push(0x38); // window descriptor: 1 << (10 + 7) = 128 KiB
        for i in 0..513 {
            let last = u32::from(i == 512);
            // Block header (3 bytes LE): bit0 Last_Block | bits1-2 Type (1 = RLE) | bits3+ Size-1.
            let header = ((131_072_u32 - 1) << 3) | (1 << 1) | last;
            f.extend_from_slice(&header.to_le_bytes()[..3]);
            f.push(0x41); // the one byte each RLE block repeats
        }
        f
    }

    /// Seal arbitrary inner payload bytes exactly the way `encrypt_listing` does, so tests can
    /// feed hand-crafted (not compressor-produced) bodies through the real decrypt path.
    fn seal_inner(browse_key: &BrowseKey, framed: &[u8]) -> String {
        let ck = conversation_key(browse_key, CRYPTO_V);
        B64.encode(encrypt_to_bytes(&ck, framed).expect("a test body seals"))
    }

    #[test]
    fn decompression_bomb_is_refused_at_the_cap() {
        // P-10 mutation: raise the cap comparison in `decompress_body`
        // (`out.len() as u64 > MAX_DECOMPRESSED_LISTING_BYTES`) to `u64::MAX` — must red this test.
        let bk: BrowseKey = rand::random();
        // The bomb goes behind the real frame byte, exactly as `compress_body` would frame it —
        // without it the frame check refuses first and the cap is never reached.
        let mut framed = vec![LISTING_PAYLOAD_V];
        framed.extend_from_slice(&crafted_rle_bomb());
        let ct = seal_inner(&bk, &framed);
        match decrypt_listing(&bk, CRYPTO_V, &ct) {
            Err(HbError::InvalidManifest(msg)) => {
                assert!(
                    msg.contains("decompression cap"),
                    "refusal must name the cap, not just fail: {msg}"
                );
            }
            other => panic!("expected InvalidManifest at the cap, got {other:?}"),
        }
    }

    #[test]
    fn precompression_v1_body_is_refused_not_dual_read() {
        // P-10 mutation: replace `Some(&other) => return Err(HbError::UnsupportedVersion(other)),`
        // in `decompress_body` with `Some(_) => {}` — must red this test (the raw JSON then reaches
        // the zstd decoder and fails as InvalidEncryptedMessage, not as the version refusal).
        let bk: BrowseKey = rand::random();
        let old = seal_v1_raw(&bk, LISTING); // v1 shape: raw JSON, no frame byte
        match decrypt_listing(&bk, CRYPTO_V, &old) {
            // `{` — the first byte of every v1 raw-JSON body — named as the refused version.
            Err(HbError::UnsupportedVersion(b'{')) => {}
            other => panic!("expected the v1 body refused by the version check, got {other:?}"),
        }
    }

    #[test]
    fn compressed_body_shrinks_the_wire_payload() {
        // P-10 mutation: make `encrypt_listing` seal `listing_json.as_bytes()` directly,
        // bypassing `compress_body` — must red this test (with NIP-44 padding + base64 overhead
        // the uncompressed body lands WIDER than the plaintext, not narrower).
        let bk: BrowseKey = rand::random();
        // 800 near-identical entries ≈ 44 KB — under the 65,408-byte NIP-44 plaintext ceiling.
        let json = format!(
            r#"{{"slug":"library","entries":[{}]}}"#,
            (0..800)
                .map(|i| {
                    let d = i % 7;
                    format!(r#"{{"name":"item {i}","path":"dir{d}/file{i}.mp4"}}"#)
                })
                .collect::<Vec<_>>()
                .join(",")
        );
        let ct = encrypt_listing(&bk, &json).unwrap();
        assert!(
            ct.len() * 4 < json.len(),
            "compressed+base64 body must be under a quarter of the plaintext ({} vs {})",
            ct.len(),
            json.len()
        );
    }

    #[test]
    fn payload_version_byte_lives_inside_the_envelope() {
        // The frame byte is the FIRST plaintext byte AFTER the NIP-44 layer — the outer payload
        // still begins with NIP-44 v2's own version byte. This pins WHERE the discriminant lives,
        // not just its value (the value is pinned in wire_freeze).
        // P-10 mutation: move `framed.push(LISTING_PAYLOAD_V)` to AFTER the zstd stream is
        // appended in `compress_body` — must red this test (the zstd magic, not the frame byte,
        // would then lead the plaintext).
        let bk: BrowseKey = rand::random();
        let ct = encrypt_listing(&bk, LISTING).unwrap();
        let raw = B64.decode(ct.as_bytes()).expect("valid base64");
        let inner = decrypt_to_bytes(&conversation_key(&bk, CRYPTO_V), &raw)
            .expect("the outer envelope is intact");
        assert_eq!(raw[0], 2, "outer envelope is NIP-44 v2");
        assert_eq!(inner[0], LISTING_PAYLOAD_V, "the body frame byte precedes the zstd stream");
    }
}
