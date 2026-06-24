//! Topics commands (M11; spec §11) — create / discover / join / leave / invite / request→approve, the
//! roster + 24h channel, and the **auto-added topic contacts**. The crypto + relay flows live in
//! `hb-core::topic` / `hb-net::topic`; this layer is the Tauri seam + the local Topic store + the
//! contact auto-add.
//!
//! **INV-2 (no listing unlock) is enforced here, both layers:** joining a Topic auto-adds each member
//! as a contact flagged [`ContactSource::Topic`] **with no browse-key** ([`upsert_topic_contact`]) —
//! so a topic contact's listings stay share-code-gated (app layer), and a browse/private-fetch keyed
//! on that contact has no browse-key to use (wire layer). Joining grants awareness + npub + teaser
//! only.

use std::time::{SystemTime, UNIX_EPOCH};

use nostr::prelude::*;
use serde::Serialize;
use tauri::State;

use hb_core::topic::{build_announce, build_public_join, new_topic, seal_membership};
use hb_core::Identity;
use hb_net::{
    approve_join, discover_public_topics, fetch_channel, fetch_invite, fetch_roster, join_public,
    join_topic, leave_topic, post_to_channel, publish_topic, request_join,
};

use crate::{
    error::{cmd_err, CmdResult},
    identity_state::SharedIdentity,
    net::{self, SharedRelay},
    store::{CachedPeer, ContactSource, DataStore, StoredTopic},
};

/// A Topic I'm in, for the UI.
#[derive(Debug, Clone, Serialize)]
pub struct TopicView {
    pub topic_id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub private: bool,
    pub joined_at: u64,
}

impl From<&StoredTopic> for TopicView {
    fn from(t: &StoredTopic) -> Self {
        Self {
            topic_id: t.meta.topic_id.clone(),
            name: t.meta.name.clone(),
            description: t.meta.description.clone(),
            tags: t.meta.tags.clone(),
            private: t.meta.private,
            joined_at: t.joined_at,
        }
    }
}

/// A discovered public Topic (non-member view): name + description + tags + a **spoofable** member
/// count. The roster identities are NOT here — those need the key (members-only).
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredTopic {
    pub topic_id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    /// Best-effort, **spoofable** count (Decision: anyone can publish a fake membership) — present it
    /// as approximate in the UI, never authoritative.
    pub member_count_estimate: usize,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

async fn me(identity: &SharedIdentity) -> Result<Identity, String> {
    identity
        .read()
        .await
        .as_ref()
        .map(|id| id.identity.clone())
        .ok_or_else(|| "No identity loaded. Generate a keypair first.".to_string())
}

/// **INV-2 app-layer — auto-add a topic contact with NO browse-key.** Adds (or, if absent) a
/// `CachedPeer` flagged [`ContactSource::Topic`] and `browse_key_hex: None`, so the member's listings
/// stay share-code-gated. An **existing** contact is left untouched — a manual contact keeps its
/// `Manual` badge and its browse-key (you added them deliberately); we never downgrade a manual add to
/// a topic add, nor strip a browse-key you already hold.
pub(crate) fn upsert_topic_contact(store: &DataStore, npub: &str) -> Result<(), String> {
    let hash = CachedPeer::pubkey_hash(npub);
    if store.load_contact(&hash).map_err(cmd_err)?.is_some() {
        return Ok(()); // already a contact (manual or topic) — never clobber
    }
    let peer = CachedPeer {
        npub: npub.to_string(),
        source: ContactSource::Topic,
        browse_key_hex: None, // INV-2: joining a Topic unlocks NO listings
        petname: None,
        profile: None,
        collections: vec![],
        online: false,
        last_fetched: chrono::Utc::now(),
        local_tags: vec![],
        // The §7 fingerprint is derivable from the npub alone (no listing access — INV-2 holds).
        fingerprint: hb_core::identity::parse_npub(npub).ok().map(|pk| hb_core::fingerprint::fingerprint(&pk)),
    };
    store.save_contact(&hash, &peer).map_err(cmd_err)
}

/// Auto-add every roster member (except me) as a topic contact.
fn auto_add_roster(store: &DataStore, roster: &[PublicKey], me_pk: &PublicKey) -> Result<(), String> {
    for pk in roster {
        if pk == me_pk {
            continue;
        }
        let npub = pk.to_bech32().map_err(cmd_err)?;
        upsert_topic_contact(store, &npub)?;
    }
    Ok(())
}

fn store_topic(store: &DataStore, t: StoredTopic) -> Result<(), String> {
    let mut topics = store.load_topics().map_err(cmd_err)?;
    if let Some(existing) = topics.iter_mut().find(|x| x.meta.topic_id == t.meta.topic_id) {
        *existing = t;
    } else {
        topics.push(t);
    }
    store.save_topics(&topics).map_err(cmd_err)
}

fn load_stored(store: &DataStore, topic_id: &str) -> Result<StoredTopic, String> {
    store
        .load_topics()
        .map_err(cmd_err)?
        .into_iter()
        .find(|t| t.meta.topic_id == topic_id)
        .ok_or_else(|| format!("You are not in topic {topic_id}"))
}

// ── commands ─────────────────────────────────────────────────────────────────────────────────────

/// List the Topics I'm in.
#[tauri::command]
pub async fn topic_list(store: State<'_, DataStore>) -> CmdResult<Vec<TopicView>> {
    Ok(store.load_topics().map_err(cmd_err)?.iter().map(TopicView::from).collect())
}

/// Create a Topic. A **public** Topic publishes an announce + a public-join credential + my membership;
/// a **private** Topic publishes only my membership (unlisted). I become its sole member.
#[tauri::command]
pub async fn topic_create(
    name: String,
    description: String,
    tags: Vec<String>,
    private: bool,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<TopicView> {
    let me = me(&identity).await?;
    // W4: a public name is validated here (root ∈ category + depth cap — backend-authoritative); a
    // private name stays freeform. A bad public path surfaces the clear hb-core error.
    let (meta, key) = new_topic(&name, &description, tags, private).map_err(cmd_err)?;
    let t = now();

    let membership = seal_membership(&key, &meta.topic_id, &me, t).map_err(cmd_err)?;
    let mut events = vec![membership.clone()];
    if !private {
        events.push(build_announce(&me, &meta, t).map_err(cmd_err)?);
        events.push(build_public_join(&me, &meta, &key, t).map_err(cmd_err)?);
    }
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    publish_topic(&client, &events).await.map_err(cmd_err)?;

    let stored = StoredTopic { meta: meta.clone(), key, joined_at: t, membership_json: Some(membership.as_json()) };
    store_topic(&store, stored.clone())?;
    Ok(TopicView::from(&stored))
}

/// Discover public Topics by tag (non-member view: name + description + the spoofable member count).
#[tauri::command]
pub async fn topic_discover(
    tags: Vec<String>,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Vec<DiscoveredTopic>> {
    let me = me(&identity).await?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    // W4: discovery is activity-ranked (member_count desc, top-N capped) inside hb-net; each entry
    // already carries its spoofable count, so no second per-topic fetch is needed here.
    let ranked = discover_public_topics(&client, &tags, net::RELAY_TIMEOUT).await.map_err(cmd_err)?;
    Ok(ranked
        .into_iter()
        .map(|(m, count)| DiscoveredTopic {
            topic_id: m.topic_id,
            name: m.name,
            description: m.description,
            tags: m.tags,
            member_count_estimate: count,
        })
        .collect())
}

/// Join a public Topic by name: obtain the key via the public-join credential, publish my membership,
/// auto-add the roster as topic contacts.
#[tauri::command]
pub async fn topic_join_public(
    name: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<TopicView> {
    let me = me(&identity).await?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    // The public-join credential is reusable (no expiry), so `seen` is not consumed; we still pass +
    // persist it so the single-use path shares one store. `&mut` lets redeem record atomically.
    let mut seen = store.load_topic_nonces().map_err(cmd_err)?;
    let t = now();
    let redeemed = join_public(&client, &name, &mut seen, t, net::RELAY_TIMEOUT).await.map_err(cmd_err)?;
    let (meta, key) = match redeemed {
        Some(v) => v,
        None => {
            return Err("Could not find a public-join credential for that Topic — is the name right?".into());
        }
    };
    let membership = join_topic(&client, &key, &meta.topic_id, &me, t).await.map_err(cmd_err)?;
    let roster = fetch_roster(&client, &meta.topic_id, &key, net::RELAY_TIMEOUT).await.unwrap_or_default();

    store.save_topic_nonces(&seen).map_err(cmd_err)?;
    auto_add_roster(&store, &roster, &me.public_key())?;
    let stored = StoredTopic { meta: meta.clone(), key, joined_at: t, membership_json: Some(membership.as_json()) };
    store_topic(&store, stored.clone())?;
    Ok(TopicView::from(&stored))
}

/// Join a private Topic by redeeming an invite addressed to me (admission path 1, redeem side).
#[tauri::command]
pub async fn topic_redeem_invite(
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Option<TopicView>> {
    let me = me(&identity).await?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    // `&mut seen`: redeem_invite atomically records a single-use invite's nonce on success (Decision E);
    // we persist the set afterward so a restart can't re-accept it.
    let mut seen = store.load_topic_nonces().map_err(cmd_err)?;
    let t = now();
    let redeemed = fetch_invite(&client, &me, &mut seen, t, net::RELAY_TIMEOUT).await.map_err(cmd_err)?;
    let (meta, key) = match redeemed {
        Some(v) => v,
        None => {
            return Ok(None);
        }
    };
    let membership = join_topic(&client, &key, &meta.topic_id, &me, t).await.map_err(cmd_err)?;
    let roster = fetch_roster(&client, &meta.topic_id, &key, net::RELAY_TIMEOUT).await.unwrap_or_default();

    store.save_topic_nonces(&seen).map_err(cmd_err)?;
    auto_add_roster(&store, &roster, &me.public_key())?;
    let stored = StoredTopic { meta: meta.clone(), key, joined_at: t, membership_json: Some(membership.as_json()) };
    store_topic(&store, stored.clone())?;
    Ok(Some(TopicView::from(&stored)))
}

/// Request to join a private Topic, sending a join-request DM to a known member.
#[tauri::command]
pub async fn topic_request_join(
    member_npub: String,
    topic_id: String,
    name: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    let me = me(&identity).await?;
    let member = hb_core::identity::parse_npub(&member_npub).map_err(cmd_err)?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    request_join(&client, &me, &member, &topic_id, &name).await.map_err(cmd_err)
}

/// Invite a peer into a Topic I'm in (member-issued invite / approve a requester). **Any** member may
/// invite (M3). Mints a sealed, single-use, expiring invite to `invitee_npub` and publishes it.
#[tauri::command]
pub async fn topic_invite(
    topic_id: String,
    invitee_npub: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    let me = me(&identity).await?;
    let invitee = hb_core::identity::parse_npub(&invitee_npub).map_err(cmd_err)?;
    let stored = load_stored(&store, &topic_id)?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    approve_join(&client, &me, &invitee, &stored.meta, &stored.key, now()).await.map_err(cmd_err)
}

/// Leave a Topic: NIP-09-retract my membership and drop the local Topic record. **Auto-added topic
/// contacts keep their flag** (they are not removed on leave/dissolution — spec §11).
#[tauri::command]
pub async fn topic_leave(
    topic_id: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    let me = me(&identity).await?;
    let stored = load_stored(&store, &topic_id)?;
    if let Some(json) = &stored.membership_json {
        let membership = Event::from_json(json).map_err(cmd_err)?;
        let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
        leave_topic(&client, &stored.key, &me.public_key(), &membership, now()).await.map_err(cmd_err)?;
    }
    let topics: Vec<StoredTopic> =
        store.load_topics().map_err(cmd_err)?.into_iter().filter(|t| t.meta.topic_id != topic_id).collect();
    store.save_topics(&topics).map_err(cmd_err)
}

/// Fetch a Topic's roster (members-only) and refresh the auto-added topic contacts.
#[tauri::command]
pub async fn topic_roster(
    topic_id: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Vec<String>> {
    let me = me(&identity).await?;
    let stored = load_stored(&store, &topic_id)?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    let roster = fetch_roster(&client, &topic_id, &stored.key, net::RELAY_TIMEOUT).await.map_err(cmd_err)?;
    auto_add_roster(&store, &roster, &me.public_key())?;
    roster.iter().map(|p| p.to_bech32().map_err(cmd_err)).collect()
}

/// A decrypted channel post for the UI.
#[derive(Debug, Clone, Serialize)]
pub struct ChannelPost {
    pub author_npub: String,
    pub body: String,
    pub ts: u64,
}

/// Read the 24h channel of a Topic I'm in (locally filtered to the last 24h).
#[tauri::command]
pub async fn topic_channel(
    topic_id: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<Vec<ChannelPost>> {
    let me = me(&identity).await?;
    let stored = load_stored(&store, &topic_id)?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    let posts = fetch_channel(&client, &topic_id, &stored.key, now(), net::RELAY_TIMEOUT).await.map_err(cmd_err)?;
    posts
        .into_iter()
        .map(|p| Ok(ChannelPost { author_npub: p.author.to_bech32().map_err(cmd_err)?, body: p.body, ts: p.ts }))
        .collect()
}

/// Post to a Topic's 24h channel.
#[tauri::command]
pub async fn topic_post(
    topic_id: String,
    body: String,
    identity: State<'_, SharedIdentity>,
    store: State<'_, DataStore>,
    relay: State<'_, SharedRelay>,
) -> CmdResult<()> {
    let me = me(&identity).await?;
    let stored = load_stored(&store, &topic_id)?;
    let client = net::client(&me, &store, &relay).await.map_err(cmd_err)?;
    post_to_channel(&client, &stored.key, &topic_id, &me, &body, now()).await.map(|_| ()).map_err(cmd_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn npub_of(id: &Identity) -> String {
        id.npub()
    }

    #[test]
    fn topic_store_round_trips_incl_meta_and_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let (meta, key) = new_topic("video/films", "criterion", vec!["video".into()], false).unwrap();
        let t = StoredTopic { meta: meta.clone(), key, joined_at: 42, membership_json: Some("{}".into()) };
        store.save_topics(&[t]).unwrap();
        let back = store.load_topics().unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].meta.topic_id, meta.topic_id);
        assert_eq!(back[0].joined_at, 42);
    }

    #[test]
    fn seen_nonce_set_persists_across_reload() {
        // Decision E: the seen-nonce set survives a restart, so an old invite can't be re-accepted.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let id = Identity::generate();
        let mut seen = store.load_topic_nonces().unwrap();
        seen.insert(hb_core::topic::invite_seen_key("topic-abc", &id.public_key()));
        store.save_topic_nonces(&seen).unwrap();
        let reloaded = store.load_topic_nonces().unwrap();
        assert!(reloaded.contains(&hb_core::topic::invite_seen_key("topic-abc", &id.public_key())));
    }

    #[test]
    fn auto_added_topic_contact_is_flagged_topic_with_no_browse_key() {
        // INV-2 (app layer): a topic contact is distinguishable (source=Topic) AND carries NO
        // browse-key — joining unlocks no listings.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let member = Identity::generate();
        upsert_topic_contact(&store, &npub_of(&member)).unwrap();
        let c = store.load_contact(&CachedPeer::pubkey_hash(&npub_of(&member))).unwrap().unwrap();
        assert_eq!(c.source, ContactSource::Topic, "auto-added contact is flagged Topic");
        assert!(c.browse_key_hex.is_none(), "a topic contact has NO browse-key (INV-2 — no listing unlock)");
    }

    #[test]
    fn upsert_never_clobbers_an_existing_manual_contact() {
        // A manual contact (with a browse-key you hold) is not downgraded to a topic add nor stripped.
        let dir = tempfile::tempdir().unwrap();
        let store = DataStore::new(dir.path().to_path_buf());
        let member = Identity::generate();
        let npub = npub_of(&member);
        let manual = CachedPeer {
            npub: npub.clone(),
            source: ContactSource::Manual,
            browse_key_hex: Some(hex::encode([7u8; 32])),
            petname: Some("hand-added".into()),
            profile: None,
            collections: vec![],
            online: false,
            last_fetched: chrono::Utc::now(),
            local_tags: vec![],
            fingerprint: None,
        };
        store.save_contact(&CachedPeer::pubkey_hash(&npub), &manual).unwrap();
        upsert_topic_contact(&store, &npub).unwrap();
        let c = store.load_contact(&CachedPeer::pubkey_hash(&npub)).unwrap().unwrap();
        assert_eq!(c.source, ContactSource::Manual, "an existing manual contact keeps its badge");
        assert!(c.browse_key_hex.is_some(), "and keeps its browse-key");
    }

    #[test]
    fn topic_contact_default_source_is_manual_on_old_data() {
        // A pre-M11 contact JSON (no `source`) loads as Manual.
        let json = r#"{"npub":"npub1xyz","browse_key_hex":null,"profile":null,"collections":[],"online":false,"last_fetched":"2026-06-23T00:00:00Z"}"#;
        let c: CachedPeer = serde_json::from_str(json).unwrap();
        assert_eq!(c.source, ContactSource::Manual);
    }
}
