//! Impersonation-resistant identity fingerprint (spec §Identity display & impersonation
//! resistance; AB4b). A deterministic word+color signature derived from an `npub`, shown beside
//! every name so two distinct keys are distinguishable at a glance even when their display names
//! collide. The petname (a local name a user binds to a key on follow) lives in the contact store;
//! this is the at-a-glance distinguisher that needs no prior contact.
//!
//! **One implementation, here in Rust** (M3 decision #7): the client and the UI must agree, so the
//! algorithm is never re-derived in TypeScript — the UI obtains a computed fingerprint over the
//! Tauri boundary, and the frontend tests pin their *rendering* to the golden vectors this module's
//! test asserts (`crates/hb-app/ui/src/lib/fingerprint_vectors.json`). Change the algorithm and the
//! golden test here goes red, forcing the fixture (and the cross-language agreement) to be updated.
//!
//! **Layering note (M3 decision #1):** this is a *display affordance*, not a Nostr protocol
//! primitive — it is never embedded in an event; it exists only to help a human tell keys apart in
//! the UI. It lives in `hb-core` for solo-dev velocity (so client + UI share one derivation);
//! candidate for extraction to an `hb-display`/`hb-app` home in M4. Do not let it justify accreting
//! other UI helpers into `hb-core`.
//!
//! **Grinding cost (QURATOR-121 #24, CWE-290):** the fingerprint renders **67 bits** of the
//! 256-bit key (5 words × 7 bits of selection + a 32-bit `#rrggbbaa` colour), so mining a key
//! whose fingerprint matches a target's is a 2^67 preimage grind. Each trial costs a secp256k1
//! key derivation (the scalar mult dominates; the byte masking is free): 2^67 ≈ 1.5×10^20
//! derivations, i.e. **~470 years even at an absurd 10^10 derivations/s** and ~5,000 years at
//! realistic GPU rates (10^8–10^9/s) — computationally infeasible for a one-time, reusable
//! impersonation, which is the level the fix targets (the classic 2^64..2^80 impracticality
//! band). That kills the old attack — a one-time ~2^36 grind reusable forever. The width matters
//! exactly where the petname defence cannot reach: the petname bound to the `npub`
//! (`ui/src/lib/identity-display.ts::petnameFor`) structurally does not exist for first contact
//! — Topic rosters, DM requests, search hits — so at first contact the fingerprint is the only
//! distinguisher and must itself be un-grindable. Honest caveat: a human comparing *at a glance*
//! resolves the five words exactly (35 bits) but the swatch only perceptually (~a dozen
//! distinguishable colours), so a glance-only comparison carries ~47 bits — hours-to-days for a
//! glance-only victim on surfaces that omit the hex; the DM-request and share-code cards render
//! the full hex, so comparing there demands all 67. The fingerprint is still a display
//! affordance, not a cryptographic boundary; a bound petname remains the stronger defence.

use nostr::prelude::PublicKey;
use serde::{Deserialize, Serialize};

/// 128 short, visually-distinct words (7 bits of selection each). Sized so 5 words + the colour
/// render 67 bits of the key (QURATOR-121 #24). Constraints the list deliberately satisfies:
/// every word is 3–7 lowercase letters, **no two share a 3-letter prefix** (so a word is identified
/// by its first three characters — a mis-read word cannot silently become another word), and no
/// 2-letter prefix has more than three members. All 16 words of the pre-widening list are retained.
const WORDS: [&str; 128] = [
    "acorn", "agate", "amber", "anchor", "anvil", "apple", "arbor", "arctic",
    "arrow", "aspen", "atlas", "aurora", "bamboo", "basalt", "beacon", "birch",
    "bismuth", "bramble", "bronze", "cedar", "chrome", "citrine", "clover", "cobalt",
    "crimson", "dahlia", "delta", "dusk", "eagle", "elixir", "ember", "emerald",
    "ether", "falcon", "fern", "fjord", "flint", "forge", "fresco", "frost",
    "gale", "garnet", "gentian", "glacier", "globe", "granite", "grotto", "halcyon",
    "harbor", "hazel", "helium", "heron", "hickory", "hollow", "hornet", "indigo",
    "iris", "ivory", "jade", "jetty", "juniper", "jute", "kayak", "kelp",
    "kestrel", "kiln", "krypton", "kudzu", "lagoon", "lapis", "larch", "ledge",
    "lemon", "lilac", "linen", "lotus", "lumen", "lunar", "luster", "mantis",
    "maple", "marble", "meadow", "mercury", "mint", "mirror", "moor", "moraine",
    "moss", "nacre", "nebula", "nickel", "nimbus", "nomad", "north", "nougat",
    "oaken", "ocean", "ochre", "onyx", "opal", "orbit", "pearl", "pepper",
    "pigeon", "quartz", "quince", "raven", "reef", "ridge", "russet", "saffron",
    "sage", "sandal", "sequoia", "slate", "spruce", "summit", "talon", "tarn",
    "thicket", "thorn", "tinsel", "topaz", "trellis", "tulip", "tundra", "umber",
];

/// A deterministic, at-a-glance fingerprint of an `npub`.
///
/// Serializes in camelCase (`{ words, colorHex }`) so the value crosses the Tauri boundary in the
/// exact shape `ui/src/lib/identity-display.ts::Fingerprint` and `fingerprint_vectors.json` already
/// use — the UI renders it verbatim, never re-deriving (M3 decision #7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fingerprint {
    /// Five words selected from well-separated key bytes.
    pub words: Vec<String>,
    /// An 8-hex-digit `#rrggbbaa` swatch from four further key bytes.
    pub color_hex: String,
}

/// Derive the fingerprint of a public key. The key bytes are already uniformly distributed (a
/// secp256k1 x-only pubkey), so bytes are sampled directly — no extra hashing needed — from
/// well-spread positions (word bytes at stride 6, colour bytes interleaved at +3) so any
/// single-byte key difference moves exactly one rendered element. Each word byte is masked to its
/// low 7 bits to index the 128-word list, so matching all five words is a 2^35 constraint and
/// matching the 4-byte colour another 2^32 — a **2^67** preimage grind in total (see the module
/// docs).
pub fn fingerprint(pk: &PublicKey) -> Fingerprint {
    let b = pk.to_bytes(); // [u8; 32]
    let words = vec![
        WORDS[(b[0] & 0x7f) as usize].to_string(),
        WORDS[(b[6] & 0x7f) as usize].to_string(),
        WORDS[(b[12] & 0x7f) as usize].to_string(),
        WORDS[(b[18] & 0x7f) as usize].to_string(),
        WORDS[(b[24] & 0x7f) as usize].to_string(),
    ];
    let color_hex = format!("#{:02x}{:02x}{:02x}{:02x}", b[3], b[9], b[15], b[21]);
    Fingerprint { words, color_hex }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    /// Fixed secret keys → their expected fingerprints. **This is the golden source of truth**
    /// shared with `ui/src/lib/fingerprint_vectors.json`: if the algorithm changes, this assertion
    /// fails first, and the JSON fixture (consumed by the frontend `identity-display` vitest) must
    /// be regenerated to match — that is how the Rust derivation and the TS rendering stay agreed.
    const GOLDEN: &[(&str, [&str; 5], &str)] = &[
        (
            "0000000000000000000000000000000000000000000000000000000000000001",
            ["thorn", "jetty", "luster", "trellis", "nacre"],
            "#7ea007ce",
        ),
        (
            "0000000000000000000000000000000000000000000000000000000000000002",
            ["larch", "tulip", "citrine", "beacon", "glacier"],
            "#9445d8ef",
        ),
    ];

    #[test]
    fn fingerprint_is_deterministic_for_an_npub() {
        let id = Identity::generate();
        let a = fingerprint(&id.public_key());
        let b = fingerprint(&id.public_key());
        assert_eq!(a, b, "the same key must always render the same fingerprint");
    }

    #[test]
    fn fingerprint_differs_for_two_distinct_keys() {
        // Two distinct keys must not collide on *both* words and color (the at-a-glance
        // distinguisher must actually distinguish). Collision on the sampled bits is ~2^-67.
        let a = fingerprint(&Identity::generate().public_key());
        let b = fingerprint(&Identity::generate().public_key());
        assert_ne!(a, b, "two distinct keys produced an identical fingerprint");
    }

    #[test]
    fn fingerprint_matches_golden_vectors() {
        // Pins the algorithm to the published cross-language fixture (decision #7). The values
        // below are also written to ui/src/lib/fingerprint_vectors.json for the frontend test.
        for (secret, words, color) in GOLDEN {
            let id = Identity::from_secret(secret).expect("valid secret");
            let fp = fingerprint(&id.public_key());
            assert_eq!(fp.words, words.to_vec(), "words drifted for secret {secret}");
            assert_eq!(&fp.color_hex, color, "color drifted for secret {secret}");
        }
    }
}
