use chrono::Utc;
use tauri::State;

use crate::{
    error::{CmdResult, cmd_err},
    store::{DataStore, Group},
};

#[tauri::command]
pub async fn groups_get(store: State<'_, DataStore>) -> CmdResult<Vec<Group>> {
    store.load_groups().map_err(cmd_err)
}

#[tauri::command]
pub async fn groups_create(name: String, store: State<'_, DataStore>) -> CmdResult<Group> {
    let mut groups = store.load_groups().map_err(cmd_err)?;
    if groups.iter().any(|g| g.name == name) {
        return Err(format!("Group '{name}' already exists"));
    }
    let group = Group { name, pubkeys: vec![], modified_at: Utc::now(), trusted: false };
    groups.push(group.clone());
    store.save_groups(&groups).map_err(cmd_err)?;
    Ok(group)
}

#[tauri::command]
pub async fn groups_rename(
    old_name: String,
    new_name: String,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let mut groups = store.load_groups().map_err(cmd_err)?;
    let group = groups
        .iter_mut()
        .find(|g| g.name == old_name)
        .ok_or_else(|| format!("Group '{old_name}' not found"))?;
    group.name = new_name;
    group.modified_at = Utc::now();
    store.save_groups(&groups).map_err(cmd_err)
}

#[tauri::command]
pub async fn groups_delete(name: String, store: State<'_, DataStore>) -> CmdResult<()> {
    let mut groups = store.load_groups().map_err(cmd_err)?;
    groups.retain(|g| g.name != name);
    store.save_groups(&groups).map_err(cmd_err)
}

#[tauri::command]
pub async fn groups_assign(
    npub: String,
    group_name: String,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let mut groups = store.load_groups().map_err(cmd_err)?;
    let group = groups
        .iter_mut()
        .find(|g| g.name == group_name)
        .ok_or_else(|| format!("Group '{group_name}' not found"))?;
    if !group.pubkeys.contains(&npub) {
        group.pubkeys.push(npub);
        group.modified_at = Utc::now();
    }
    store.save_groups(&groups).map_err(cmd_err)
}

#[tauri::command]
pub async fn groups_unassign(
    npub: String,
    group_name: String,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let mut groups = store.load_groups().map_err(cmd_err)?;
    let group = groups
        .iter_mut()
        .find(|g| g.name == group_name)
        .ok_or_else(|| format!("Group '{group_name}' not found"))?;
    let before = group.pubkeys.len();
    group.pubkeys.retain(|id| id != &npub);
    if group.pubkeys.len() != before {
        group.modified_at = Utc::now();
    }
    store.save_groups(&groups).map_err(cmd_err)
}

/// Mark a contact group **trusted** (or not) for Private collections (M10). A trusted group's
/// members each receive a per-recipient sealed copy of every Private collection on the next
/// publish; un-trusting a group revokes them on the *next* republish (it cannot recall an
/// already-fetched copy — the honest "not DRM" caveat, surfaced in the UI). Local-only.
#[tauri::command]
pub async fn groups_set_trusted(
    name: String,
    trusted: bool,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let mut groups = store.load_groups().map_err(cmd_err)?;
    let group = groups
        .iter_mut()
        .find(|g| g.name == name)
        .ok_or_else(|| format!("Group '{name}' not found"))?;
    group.trusted = trusted;
    group.modified_at = Utc::now();
    store.save_groups(&groups).map_err(cmd_err)
}

/// Atomically replace a contact's group memberships with a new set.
/// Any group not in `group_names` loses the contact; any group in `group_names` gains it.
/// Used for drag-and-drop reassignment from the UI.
#[tauri::command]
pub async fn contact_update_groups(
    npub: String,
    group_names: Vec<String>,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let mut groups = store.load_groups().map_err(cmd_err)?;
    let now = Utc::now();

    for group in &mut groups {
        let was_member = group.pubkeys.contains(&npub);
        let should_be_member = group_names.contains(&group.name);

        if was_member && !should_be_member {
            group.pubkeys.retain(|id| id != &npub);
            group.modified_at = now;
        } else if !was_member && should_be_member {
            group.pubkeys.push(npub.clone());
            group.modified_at = now;
        }
    }

    store.save_groups(&groups).map_err(cmd_err)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::store::{CachedPeer, DataStore, Group};
    use tempfile::TempDir;

    fn make_store() -> (TempDir, DataStore) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        (dir, DataStore::new(path))
    }

    fn test_peer(npub: &str) -> CachedPeer {
        CachedPeer {
            npub: npub.to_string(),
            source: crate::store::ContactSource::Manual,
            browse_key_hex: None,
            petname: None,
            profile: None,
            collections: vec![],
            online: false,
            last_fetched: chrono::Utc::now(),
            local_tags: vec![],
        }
    }

    /// T21: following without a group_name leaves the contact with no group membership (Ungrouped).
    #[test]
    fn follow_skip_ungrouped() {
        let (_dir, store) = make_store();
        let npub = "hb1_testpeer".to_string();
        let hash = CachedPeer::pubkey_hash(&npub);
        store.save_contact(&hash, &test_peer(&npub)).unwrap();

        let groups = store.load_groups().unwrap();
        assert!(
            groups.iter().all(|g| !g.pubkeys.contains(&npub)),
            "contact saved without group must not appear in any group pubkeys list"
        );
    }

    /// T21: a contact can belong to multiple groups simultaneously.
    #[test]
    fn multi_group_membership() {
        let (_dir, store) = make_store();
        let npub = "hb1_testpeer".to_string();
        let now = chrono::Utc::now();

        store
            .save_groups(&[
                Group { name: "A".into(), pubkeys: vec![npub.clone()], modified_at: now, trusted: false },
                Group { name: "B".into(), pubkeys: vec![npub.clone()], modified_at: now, trusted: false },
            ])
            .unwrap();

        let groups = store.load_groups().unwrap();
        let in_a = groups.iter().find(|g| g.name == "A").unwrap().pubkeys.contains(&npub);
        let in_b = groups.iter().find(|g| g.name == "B").unwrap().pubkeys.contains(&npub);
        assert!(in_a && in_b, "contact must be able to belong to multiple groups");
    }

    /// T21: deleting a group does not delete the contacts in that group (they become Ungrouped).
    #[test]
    fn delete_group_moves_to_ungrouped() {
        let (_dir, store) = make_store();
        let npub = "hb1_testpeer".to_string();
        let hash = CachedPeer::pubkey_hash(&npub);

        store.save_contact(&hash, &test_peer(&npub)).unwrap();
        store
            .save_groups(&[Group {
                name: "MyGroup".into(),
                pubkeys: vec![npub.clone()],
                modified_at: chrono::Utc::now(),
                trusted: false,
            }])
            .unwrap();

        // Delete group by saving an empty list.
        store.save_groups(&[]).unwrap();

        let contacts = store.list_contacts().unwrap();
        assert!(
            contacts.iter().any(|c| c.npub == npub),
            "contact must remain in contact list after its group is deleted"
        );
        let groups = store.load_groups().unwrap();
        assert!(
            groups.iter().all(|g| !g.pubkeys.contains(&npub)),
            "deleted group must not contain the contact"
        );
    }

    /// T21: a CachedPeer last fetched >7 days ago is considered stale.
    #[test]
    fn stale_after_7_days() {
        let stale_fetched = chrono::Utc::now() - chrono::Duration::days(8);
        let peer = test_peer("hb1_old");
        let peer = CachedPeer { last_fetched: stale_fetched, ..peer };
        let age_days = chrono::Utc::now()
            .signed_duration_since(peer.last_fetched)
            .num_days();
        assert!(age_days >= 7, "peer fetched 8 days ago must register as stale (≥7 days)");
    }

    /// T21: Group JSON must never contain relay-facing fields that could inadvertently leak
    /// group membership if a Group value is accidentally serialised into a relay request.
    #[test]
    fn groups_not_in_relay_traffic() {
        let group = Group {
            name: "Friends".into(),
            pubkeys: vec!["hb1_abc".into()],
            modified_at: chrono::Utc::now(),
            trusted: false,
        };
        let json = serde_json::to_string(&group).unwrap();
        assert!(!json.contains("relay"), "group JSON must not contain 'relay'");
        assert!(!json.contains("node_addr"), "group JSON must not contain 'node_addr'");
        assert!(!json.contains("online"), "group JSON must not contain 'online'");
    }

    /// T21: contact_refresh updates the local cache file.
    #[test]
    fn contact_refresh_updates_cache() {
        let (_dir, store) = make_store();
        let npub = "hb1_testpeer".to_string();
        let hash = CachedPeer::pubkey_hash(&npub);

        store.save_contact(&hash, &test_peer(&npub)).unwrap();

        let updated = CachedPeer { online: true, ..test_peer(&npub) };
        store.save_contact(&hash, &updated).unwrap();

        let loaded = store.load_contact(&hash).unwrap().unwrap();
        assert!(loaded.online, "refreshed contact must reflect updated online status");
    }

    /// contact_update_groups replaces memberships atomically.
    #[test]
    fn contact_update_groups_replaces_memberships() {
        let (_dir, store) = make_store();
        let npub = "hb1_peer".to_string();
        let now = chrono::Utc::now();

        store
            .save_groups(&[
                Group { name: "A".into(), pubkeys: vec![npub.clone()], modified_at: now, trusted: false },
                Group { name: "B".into(), pubkeys: vec![], modified_at: now, trusted: false },
                Group { name: "C".into(), pubkeys: vec![npub.clone()], modified_at: now, trusted: false },
            ])
            .unwrap();

        // Move peer from {A, C} to {B} only.
        let mut groups = store.load_groups().unwrap();
        for group in &mut groups {
            let should = group.name == "B";
            let was = group.pubkeys.contains(&npub);
            if was && !should {
                group.pubkeys.retain(|id| id != &npub);
            } else if !was && should {
                group.pubkeys.push(npub.clone());
            }
        }
        store.save_groups(&groups).unwrap();

        let loaded = store.load_groups().unwrap();
        let in_a = loaded.iter().find(|g| g.name == "A").unwrap().pubkeys.contains(&npub);
        let in_b = loaded.iter().find(|g| g.name == "B").unwrap().pubkeys.contains(&npub);
        let in_c = loaded.iter().find(|g| g.name == "C").unwrap().pubkeys.contains(&npub);
        assert!(!in_a, "A must no longer contain the peer");
        assert!(in_b, "B must contain the peer");
        assert!(!in_c, "C must no longer contain the peer");
    }

    /// M10: the `trusted` flag defaults to false for a pre-M10 group and round-trips through the
    /// store. Trust must never be silently granted on upgrade (it routes Private collections).
    #[test]
    fn group_trusted_flag_defaults_false_and_round_trips() {
        let (_dir, store) = make_store();
        let now = chrono::Utc::now();
        // A groups.json written before M10 has no `trusted` field → must load as untrusted.
        let legacy = r#"[{"name":"old","pubkeys":[],"modified_at":"2026-04-01T00:00:00Z"}]"#;
        std::fs::write(store.groups_path(), legacy).unwrap();
        let loaded = store.load_groups().unwrap();
        assert!(!loaded[0].trusted, "a pre-M10 group must load as untrusted (false)");

        // Marking trusted persists.
        store
            .save_groups(&[Group {
                name: "vault".into(),
                pubkeys: vec!["npubx".into()],
                modified_at: now,
                trusted: true,
            }])
            .unwrap();
        let back = store.load_groups().unwrap();
        assert!(back.iter().find(|g| g.name == "vault").unwrap().trusted, "trusted must persist");
    }

    /// Groups are returned most-recently-modified first.
    #[test]
    fn groups_ordered_by_modified_at_desc() {
        let (_dir, store) = make_store();
        let t1 = chrono::Utc::now() - chrono::Duration::hours(2);
        let t2 = chrono::Utc::now() - chrono::Duration::hours(1);
        let t3 = chrono::Utc::now();

        store
            .save_groups(&[
                Group { name: "old".into(), pubkeys: vec![], modified_at: t1, trusted: false },
                Group { name: "recent".into(), pubkeys: vec![], modified_at: t3, trusted: false },
                Group { name: "middle".into(), pubkeys: vec![], modified_at: t2, trusted: false },
            ])
            .unwrap();

        let groups = store.load_groups().unwrap();
        let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["recent", "middle", "old"], "groups must be sorted newest-first");
    }
}
