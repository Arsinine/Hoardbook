//! Private-collection **browse** (M10) — the trusted-viewer side of §Private Collections. A peer who
//! has marked you trusted publishes a Private collection sealed to your `npub`; this command fetches
//! the gift-wrapped events addressed to you, opens those from authors you follow (your contacts are
//! the allowlist — the post-decrypt inner-author check), and renders each as a `Collection` for the
//! UI. A non-trusted viewer simply has **nothing to fetch** — there is no locked-teaser hint (unlike
//! a public listing browsed without the share code).

use nostr::prelude::ToBech32;
use serde::Serialize;
use tauri::State;

use hb_core::types::Collection;
use hb_net::fetch_private_listings;

use crate::{
    error::{cmd_err, CmdResult},
    identity_state::SharedIdentity,
    net,
    store::DataStore,
};

/// A trusted peer's decrypted Private collections, grouped under their `npub` for the UI.
#[derive(Debug, Clone, Serialize)]
pub struct PrivatePeerCollections {
    pub npub: String,
    pub collections: Vec<Collection>,
}

/// Convert a decrypted private-listing JSON back into a `Collection` — the inverse of
/// `collection_to_listing_json` (`entries` → `listing`). Pure — unit-tested without a relay.
pub(crate) fn private_listing_to_collection(listing_json: &str) -> Result<Collection, String> {
    let mut v: serde_json::Value = serde_json::from_str(listing_json).map_err(cmd_err)?;
    if let serde_json::Value::Object(ref mut map) = v {
        if let Some(entries) = map.remove("entries") {
            map.insert("listing".into(), entries);
        }
    }
    serde_json::from_value(v).map_err(cmd_err)
}

/// The allowlist of authors whose Private collections we accept: every contact we follow (by `npub`).
/// The post-decrypt inner-author check uses this — a sealed listing from a stranger we don't follow
/// is dropped even though it was addressed to us (it can't, then, force unsolicited content on us).
///
/// **Intentional send/receive asymmetry (chorus M10).** *Sending* a Private collection seals it to
/// the members of your **trusted groups**; *receiving* one accepts only authors **you follow**. These
/// are deliberately different sets: the receive side is an anti-unsolicited-content gate, so a peer
/// who marks *you* trusted but whom *you* have not followed is **silently dropped** — to read A's
/// Private collection, follow A. This errs toward rejection (never a security risk), and the
/// asymmetry is the point, not a bug.
pub(crate) fn contact_author_allowlist(store: &DataStore) -> Vec<nostr::PublicKey> {
    store
        .list_contacts()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|c| hb_core::identity::parse_npub(&c.npub).ok())
        .collect()
}

/// Fetch + decrypt the Private collections trusted peers have sealed to me, grouped by author.
#[tauri::command]
pub async fn browse_private_collections(
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
) -> CmdResult<Vec<PrivatePeerCollections>> {
    let me = {
        let guard = identity.read().await;
        guard
            .as_ref()
            .map(|id| id.identity.clone())
            .ok_or("No identity loaded. Generate a keypair first.")?
    };
    let allowlist = contact_author_allowlist(&store);
    if allowlist.is_empty() {
        return Ok(vec![]); // no followed authors → nothing to accept (and nothing leaks)
    }

    let client = net::connect(&me, &store).await.map_err(cmd_err)?;
    let opened = fetch_private_listings(&client, &me, &allowlist, net::RELAY_TIMEOUT).await;
    client.disconnect().await;
    let opened = opened.map_err(cmd_err)?;

    // Group the decrypted listings under the inner author's npub for the UI.
    let mut by_author: std::collections::BTreeMap<String, Vec<Collection>> =
        std::collections::BTreeMap::new();
    for o in opened {
        let npub = o.inner_author.to_bech32().expect("a valid public key always encodes to an npub");
        if let Ok(col) = private_listing_to_collection(&o.listing_json) {
            by_author.entry(npub).or_default().push(col);
        }
    }
    Ok(by_author
        .into_iter()
        .map(|(npub, collections)| PrivatePeerCollections { npub, collections })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::collection::collection_to_listing_json;
    use hb_core::types::{DirectoryItem, ItemType, Visibility};

    #[test]
    fn listing_json_round_trips_back_to_a_private_collection() {
        // The exact JSON a publisher seals (collection_to_listing_json) must reverse to an
        // equivalent Collection on the trusted viewer's side, preserving the Private visibility.
        let col = Collection {
            slug: "vault".into(),
            path_alias: "The Vault".into(),
            description: Some("rare".into()),
            item_count: 1,
            est_size: None,
            content_types: vec!["video".into()],
            tags: vec!["rare".into()],
            languages: vec![],
            visibility: Visibility::Private,
            last_updated: chrono::Utc::now(),
            listing: vec![DirectoryItem {
                name: "rare.mkv".into(),
                item_type: ItemType::File,
                size: Some("9GB".into()),
                format: Some("MKV".into()),
                year: None,
                tags: vec![],
                note: None,
                children: vec![],
            }],
        };
        let json = collection_to_listing_json(&col).unwrap();
        assert!(json.contains("\"entries\""), "the sealed form uses `entries`");
        let back = private_listing_to_collection(&json).unwrap();
        assert_eq!(back.slug, "vault");
        assert_eq!(back.path_alias, "The Vault");
        assert_eq!(back.visibility, Visibility::Private, "the decrypted collection stays Private");
        assert_eq!(back.listing.len(), 1);
        assert_eq!(back.listing[0].name, "rare.mkv");
    }

    #[test]
    fn malformed_listing_json_is_a_reasoned_err() {
        assert!(private_listing_to_collection("not json").is_err());
        assert!(private_listing_to_collection("{}").is_err(), "missing required Collection fields");
    }
}
