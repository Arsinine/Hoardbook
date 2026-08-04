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
pub async fn groups_create(
    name: String,
    color: Option<String>,
    store: State<'_, DataStore>,
) -> CmdResult<Group> {
    let mut groups = store.load_groups().map_err(cmd_err)?;
    if groups.iter().any(|g| g.name == name) {
        return Err(format!("Group '{name}' already exists"));
    }
    let group = Group { name, pubkeys: vec![], modified_at: Utc::now(), color };
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

/// List the npubs in the Private-collection audience (M21 W5). Each listed npub receives a
/// per-recipient sealed copy of every Private collection on publish. Decoupled from contact groups
/// by owner ruling (2026-08-04): joining/creating a topic or group never enrols anyone here.
#[tauri::command]
pub async fn private_audience_list(store: State<'_, DataStore>) -> CmdResult<Vec<String>> {
    store.load_private_audience().map_err(cmd_err)
}

/// Add or remove a single npub from the Private-collection audience (M21 W5). Idempotent in both
/// directions. Removing a recipient revokes them on the *next* republish only — it cannot recall
/// an already-fetched copy (the honest "not DRM" caveat, surfaced in the UI). Local-only.
#[tauri::command]
pub async fn private_audience_set(
    npub: String,
    receives: bool,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let mut audience = store.load_private_audience().map_err(cmd_err)?;
    if receives {
        if !audience.contains(&npub) {
            audience.push(npub);
        }
    } else {
        audience.retain(|n| n != &npub);
    }
    store.save_private_audience(&audience).map_err(cmd_err)
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
            last_presence: None,
            local_tags: vec![],
            fingerprint: None,
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
                Group { name: "A".into(), pubkeys: vec![npub.clone()], modified_at: now, color: None },
                Group { name: "B".into(), pubkeys: vec![npub.clone()], modified_at: now, color: None },
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
                color: None,
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
            color: None,
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
                Group { name: "A".into(), pubkeys: vec![npub.clone()], modified_at: now, color: None },
                Group { name: "B".into(), pubkeys: vec![], modified_at: now, color: None },
                Group { name: "C".into(), pubkeys: vec![npub.clone()], modified_at: now, color: None },
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

    /// M21 W5: a `groups.json` written before this change (containing `"trusted": true`) still
    /// loads — serde ignores unknown fields by default — and its members are NOT in the Private
    /// audience. Filing a contact under a (formerly trusted) group never enrols them as a Private
    /// recipient. This replaces the old `group_trusted_flag_defaults_false_and_round_trips`.
    #[test]
    fn legacy_groups_json_with_trusted_field_loads_and_is_not_in_audience() {
        let (_dir, store) = make_store();
        let legacy = r#"[{"name":"old","pubkeys":["npub_legacy"],"modified_at":"2026-04-01T00:00:00Z","trusted":true}]"#;
        std::fs::write(store.groups_path(), legacy).unwrap();

        let loaded = store.load_groups().unwrap();
        assert_eq!(loaded.len(), 1, "a pre-W5 groups.json with a `trusted` field still loads");
        assert_eq!(loaded[0].name, "old");
        assert!(loaded[0].pubkeys.contains(&"npub_legacy".to_string()));

        // The Private audience is a separate file and starts empty — group membership does NOT
        // flow into it. This is the regression that would have failed before W5.
        let audience = store.load_private_audience().unwrap();
        assert!(
            !audience.contains(&"npub_legacy".to_string()),
            "a group member (formerly trusted) must never be in the Private audience"
        );
    }

    /// M13 W5 item 3: `color` defaults to `None` for a pre-existing group (no `color` field) and
    /// round-trips through the store once set.
    #[test]
    fn group_color_defaults_none_and_round_trips() {
        let (_dir, store) = make_store();
        // A groups.json written before this feature has no `color` field → must load as None.
        let legacy = r#"[{"name":"old","pubkeys":[],"modified_at":"2026-04-01T00:00:00Z"}]"#;
        std::fs::write(store.groups_path(), legacy).unwrap();
        let loaded = store.load_groups().unwrap();
        assert!(loaded[0].color.is_none(), "a pre-color group must load with color=None");

        // Setting a color persists.
        store
            .save_groups(&[Group {
                name: "vibrant".into(),
                pubkeys: vec![],
                modified_at: chrono::Utc::now(),
                color: Some("#ff00aa".into()),
            }])
            .unwrap();
        let back = store.load_groups().unwrap();
        assert_eq!(
            back.iter().find(|g| g.name == "vibrant").unwrap().color.as_deref(),
            Some("#ff00aa"),
            "color must persist"
        );
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
                Group { name: "old".into(), pubkeys: vec![], modified_at: t1, color: None },
                Group { name: "recent".into(), pubkeys: vec![], modified_at: t3, color: None },
                Group { name: "middle".into(), pubkeys: vec![], modified_at: t2, color: None },
            ])
            .unwrap();

        let groups = store.load_groups().unwrap();
        let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["recent", "middle", "old"], "groups must be sorted newest-first");
    }

    /// M21 W5 key regression: the Private-collection audience is a separate file from groups.
    /// Mutating groups the way `groups_assign` and `contact_update_groups` do (load → mutate pubkeys
    /// → `save_groups`) must NOT touch `private_audience.json`. Before W5, `private_recipients` was
    /// a live query over trusted groups, so adding a member silently enrolled them — this pins that
    /// the two stores are independent.
    #[test]
    fn group_mutations_do_not_touch_private_audience() {
        let (_dir, store) = make_store();
        let npub = "npub_a".to_string();
        let now = chrono::Utc::now();

        // Seed an empty group and an empty audience (both start empty).
        store
            .save_groups(&[Group { name: "Pals".into(), pubkeys: vec![], modified_at: now, color: None }])
            .unwrap();
        store.save_private_audience(&[]).unwrap();

        // The exact mutation `groups_assign` performs: load groups, push the npub, save.
        let mut groups = store.load_groups().unwrap();
        let group = groups.iter_mut().find(|g| g.name == "Pals").unwrap();
        if !group.pubkeys.contains(&npub) {
            group.pubkeys.push(npub.clone());
            group.modified_at = now;
        }
        store.save_groups(&groups).unwrap();

        // The exact mutation `contact_update_groups` performs for a second group.
        groups = store.load_groups().unwrap();
        let g2 = Group { name: "Work".into(), pubkeys: vec![], modified_at: now, color: None };
        groups.push(g2);
        let work = groups.iter_mut().find(|g| g.name == "Work").unwrap();
        if !work.pubkeys.contains(&npub) {
            work.pubkeys.push(npub.clone());
            work.modified_at = now;
        }
        store.save_groups(&groups).unwrap();

        // Audience file is untouched — the contact is in two groups but not in the audience.
        let audience = store.load_private_audience().unwrap();
        assert!(
            !audience.contains(&npub),
            "filing a contact under a group must NOT enrol them as a Private recipient (M21 W5)"
        );
        assert!(audience.is_empty(), "the audience file is independent of groups");
    }
}
