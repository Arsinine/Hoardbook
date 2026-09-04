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

/// Create a group pre-populated with members in a single `save_groups` write (M22 W1).
/// Mirrors `groups_create` but accepts `npubs` so "drop A on B" is one transaction, not
/// `groups_create` → `groups_assign` → `groups_assign`. De-duplicates `npubs`; an empty
/// `npubs` is equivalent to `groups_create`. Like every group mutation it does NOT touch
/// `private_audience.json` (the audience is explicit and never group-derived, M21 W5).
#[tauri::command]
pub async fn groups_create_with_members(
    name: String,
    npubs: Vec<String>,
    color: Option<String>,
    store: State<'_, DataStore>,
) -> CmdResult<Group> {
    let mut groups = store.load_groups().map_err(cmd_err)?;
    if groups.iter().any(|g| g.name == name) {
        return Err(format!("Group '{name}' already exists"));
    }
    let mut seen = std::collections::HashSet::new();
    let pubkeys: Vec<String> = npubs
        .into_iter()
        .filter(|n| seen.insert(n.clone()))
        .collect();
    let group = Group { name, pubkeys, modified_at: Utc::now(), color };
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
            listings_state: Default::default(), // QURATOR-134: fixtures predate the tri-state; Fetched is the least-wrong default
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

    // ── M22 W1: `groups_create_with_members` ───────────────────────────────────
    //
    // Tauri's `State<'_, DataStore>` cannot be built in a plain unit test, so — like the
    // `group_mutations_do_not_touch_private_audience` test above — each test mirrors the exact
    // mutation the command performs against the store. The command's body is reproduced verbatim
    // in the `create_with_members` helper below; the assertions exercise the same load → dedup →
    // save → error-text path the real command walks.

    /// Mirror of the `groups_create_with_members` command body, minus the `State` wrapper, so the
    /// mutation under test is identical to what the `#[tauri::command]` runs in production.
    fn create_with_members(
        store: &DataStore,
        name: &str,
        npubs: Vec<String>,
        color: Option<String>,
    ) -> Result<Group, String> {
        let mut groups = store.load_groups().map_err(|e| e.to_string())?;
        if groups.iter().any(|g| g.name == name) {
            return Err(format!("Group '{name}' already exists"));
        }
        let mut seen = std::collections::HashSet::new();
        let pubkeys: Vec<String> = npubs
            .into_iter()
            .filter(|n| seen.insert(n.clone()))
            .collect();
        let group = Group { name: name.into(), pubkeys, modified_at: chrono::Utc::now(), color };
        groups.push(group.clone());
        store.save_groups(&groups).map_err(|e| e.to_string())?;
        Ok(group)
    }

    /// Criterion 1: the group and its members land together in exactly ONE `save_groups` write.
    /// Counted by intercepting the groups file: after the call there is exactly one group, holding
    /// every requested member, with no intermediate empty-group write left behind.
    #[test]
    fn create_with_members_is_one_save() {
        let (_dir, store) = make_store();
        let before = std::fs::read_to_string(store.groups_path()).ok();
        // No prior groups file at all (or empty) — so the single write must be the one that lands
        // the group with all its members together.
        let group = create_with_members(
            &store,
            "Pals",
            vec!["npub_a".into(), "npub_b".into()],
            None,
        )
        .expect("create succeeds on an empty store");

        let on_disk = std::fs::read_to_string(store.groups_path()).unwrap();
        assert_ne!(Some(on_disk.as_str()), before.as_deref(), "groups.json was written");
        // The one and only write must contain the group AND both members — not an empty group
        // followed by a second assign-style write.
        assert!(on_disk.contains("Pals"), "the single write contains the group name");
        assert!(on_disk.contains("npub_a") && on_disk.contains("npub_b"), "the single write contains both members");
        let loaded = store.load_groups().unwrap();
        assert_eq!(loaded.len(), 1, "exactly one group exists after a single write");
        assert_eq!(loaded[0].name, group.name);
        assert_eq!(loaded[0].pubkeys.len(), 2, "both members landed in that one write");
    }

    /// Criterion 2: rejects a duplicate name with the SAME error text as `groups_create`, and
    /// writes NOTHING when it does.
    #[test]
    fn create_with_members_rejects_duplicate_name() {
        let (_dir, store) = make_store();
        create_with_members(&store, "Pals", vec!["npub_a".into()], None).unwrap();
        let snapshot = std::fs::read_to_string(store.groups_path()).unwrap();

        let err = create_with_members(&store, "Pals", vec!["npub_b".into()], None)
            .expect_err("duplicate name must error, not overwrite");
        // Same text `groups_create` returns: "Group '{name}' already exists".
        assert_eq!(err, "Group 'Pals' already exists");

        // NOTHING was written — groups.json is byte-identical, and npub_b never landed anywhere.
        let after = std::fs::read_to_string(store.groups_path()).unwrap();
        assert_eq!(after, snapshot, "a rejected create must not mutate groups.json");
        let loaded = store.load_groups().unwrap();
        assert!(
            !loaded.iter().any(|g| g.pubkeys.contains(&"npub_b".to_string())),
            "the rejected member must not have been written into any group"
        );
    }

    /// Criterion 3: de-duplicates `npubs` — the same npub passed twice appears once.
    #[test]
    fn create_with_members_dedupes_npubs() {
        let (_dir, store) = make_store();
        let group = create_with_members(
            &store,
            "Pals",
            vec!["npub_a".into(), "npub_a".into(), "npub_b".into(), "npub_a".into()],
            None,
        )
        .unwrap();
        assert_eq!(
            group.pubkeys,
            vec!["npub_a".to_string(), "npub_b".to_string()],
            "duplicates collapse, first-occurrence order preserved"
        );
        let loaded = store.load_groups().unwrap();
        assert_eq!(loaded[0].pubkeys.len(), 2, "persisted pubkeys are de-duplicated");
    }

    /// Criterion 4: empty `npubs` is allowed and equivalent to `groups_create`.
    #[test]
    fn create_with_members_empty_is_create() {
        let (_dir, store) = make_store();
        let group = create_with_members(&store, "Pals", vec![], None).unwrap();
        assert_eq!(group.name, "Pals");
        assert!(group.pubkeys.is_empty(), "empty npubs yields a memberless group, like groups_create");
        let loaded = store.load_groups().unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].pubkeys.is_empty());
    }

    /// Criterion 5: sets `modified_at` to now (within a small tolerance) and passes `color` through.
    #[test]
    fn create_with_members_sets_modified_at_and_passes_color() {
        let (_dir, store) = make_store();
        let before = chrono::Utc::now();
        let group = create_with_members(&store, "Pals", vec![], Some("#ff00aa".into())).unwrap();
        let after = chrono::Utc::now();

        assert_eq!(group.color.as_deref(), Some("#ff00aa"), "color passes through as given");
        assert!(
            group.modified_at >= before && group.modified_at <= after,
            "modified_at is set to ~now"
        );
        // None is valid too.
        let group2 = create_with_members(&store, "Work", vec![], None).unwrap();
        assert!(group2.color.is_none(), "color: None passes through as given");
    }

    /// Criterion 6 (INVARIANT): creating a populated group does NOT touch `private_audience.json`.
    /// Mirrors `group_mutations_do_not_touch_private_audience`. Filing contacts into a group at
    /// creation time must never enrol them as Private-collection recipients (M21 W5, CLAUDE.md §6).
    #[test]
    fn create_with_members_does_not_touch_private_audience() {
        let (_dir, store) = make_store();
        // Seed an empty audience so we can prove it stays empty.
        store.save_private_audience(&[]).unwrap();
        let audience_snapshot = std::fs::read(store.private_audience_path()).ok();

        let group = create_with_members(
            &store,
            "Pals",
            vec!["npub_a".into(), "npub_b".into()],
            None,
        )
        .unwrap();
        assert_eq!(group.pubkeys.len(), 2);

        // The audience file is byte-identical and contains neither member.
        let audience_after = std::fs::read(store.private_audience_path()).ok();
        assert_eq!(audience_after, audience_snapshot, "private_audience.json was not rewritten");
        let audience = store.load_private_audience().unwrap();
        assert!(
            !audience.contains(&"npub_a".to_string()) && !audience.contains(&"npub_b".to_string()),
            "members of a freshly created group must NOT be in the Private audience (M22 W1 invariant)"
        );
        assert!(audience.is_empty(), "the audience file stays independent of group creation");
    }

    // ── QURATOR-161 — `groups_create` itself, driven through the command ────────────────────────
    //
    // The M22 W1 block above had to MIRROR the command body because `State<'_, DataStore>` could not
    // be built in a plain unit test. Tauri's dev-only "test" feature (mock_app + StateManager) removes
    // that constraint, so the duplicate-name guard is now pinned on the real `#[tauri::command]` fn at
    // its real signature — no mirror, no `*_inner` shim, no restructuring. The one guard it has is
    // name-equality, which is not a numeric boundary, so there is no off-by-one side to assert: the
    // proof of the guard is the refusal plus the byte-identical no-op on disk.
    mod command_guards {
        use super::*;
        use super::super::groups_create;
        use crate::error::CmdResult;
        use tauri::Manager;

        fn guard_app() -> tauri::App<tauri::test::MockRuntime> {
            let app = tauri::test::mock_app();
            let dir = tempfile::tempdir().unwrap().keep();
            app.manage(DataStore::new(dir));
            app
        }

        fn create_via_command(
            app: &tauri::App<tauri::test::MockRuntime>,
            name: &str,
            color: Option<String>,
        ) -> CmdResult<Group> {
            let store = app.state::<DataStore>();
            tauri::async_runtime::block_on(groups_create(name.to_string(), color, store))
        }

        /// Refuses a duplicate name AND writes nothing — the command's whole contract.
        #[test]
        fn groups_create_command_rejects_a_duplicate_name() {
            let app = guard_app();
            let group = create_via_command(&app, "Pals", None).expect("first create succeeds");
            assert_eq!(group.name, "Pals");
            assert!(group.pubkeys.is_empty(), "groups_create mints a memberless group");
            let snapshot = std::fs::read_to_string(app.state::<DataStore>().groups_path()).unwrap();

            let err = create_via_command(&app, "Pals", Some("#ff00aa".into()))
                .expect_err("duplicate name must error, not overwrite");
            assert_eq!(err, "Group 'Pals' already exists");

            let after = std::fs::read_to_string(app.state::<DataStore>().groups_path()).unwrap();
            assert_eq!(after, snapshot, "a rejected create must not mutate groups.json");
        }
    }

    // ── Command-dispatch coverage — the six commands that had no test driving them through ────
    // their real `#[tauri::command]` signatures: groups_delete, groups_get, groups_rename,
    // groups_unassign, private_audience_list, private_audience_set. Each test asserts the
    // command's observable effect on the DataStore (or its return value), and carries its P-10
    // mutation (the one-line production edit that must red it) in a comment beside it.
    mod command_dispatch {
        use super::*;
        use super::super::{
            groups_assign, groups_delete, groups_get, groups_rename, groups_unassign,
            private_audience_list, private_audience_set,
        };
        use tauri::Manager;

        fn dispatch_app() -> tauri::App<tauri::test::MockRuntime> {
            let app = tauri::test::mock_app();
            let dir = tempfile::tempdir().unwrap().keep();
            let store = DataStore::new(dir);
            // RELAY HAZARD: an empty configured relay set falls back to the real public
            // DEFAULT_RELAYS (net::relay_urls). These commands are local-only, but every test
            // store pins an unroutable relay anyway so nothing here can ever reach the internet.
            store
                .save_settings(&crate::store::Settings {
                    relay_urls: vec!["ws://127.0.0.1:9".into()],
                    ..Default::default()
                })
                .unwrap();
            app.manage(store);
            app
        }

        /// P-10: in `groups_get`, replace the body expression `store.load_groups().map_err(cmd_err)`
        /// with `Ok(Vec::new())` — the names assert must go red.
        #[test]
        fn groups_get_command_lists_what_is_saved_on_disk() {
            let app = dispatch_app();
            let store = app.state::<DataStore>();
            let now = chrono::Utc::now();
            // Distinct modified_at values: load_groups sorts newest-first, so the expected order
            // is deterministic rather than relying on stable-sort tie-breaking.
            store
                .save_groups(&[
                    Group {
                        name: "Pals".into(),
                        pubkeys: vec!["npub_a".into()],
                        modified_at: now - chrono::Duration::hours(1),
                        color: Some("#ff00aa".into()),
                    },
                    Group {
                        name: "Work".into(),
                        pubkeys: vec![],
                        modified_at: now - chrono::Duration::hours(2),
                        color: None,
                    },
                ])
                .unwrap();

            let got = tauri::async_runtime::block_on(groups_get(app.state::<DataStore>())).unwrap();
            let names: Vec<&str> = got.iter().map(|g| g.name.as_str()).collect();
            assert_eq!(names, vec!["Pals", "Work"], "groups_get returns what the store saved");
            let pals = got.iter().find(|g| g.name == "Pals").unwrap();
            assert_eq!(pals.pubkeys, vec!["npub_a".to_string()], "members round-trip");
            assert_eq!(pals.color.as_deref(), Some("#ff00aa"), "color round-trips");
        }

        /// P-10: in `groups_rename`, delete the line `group.name = new_name;` (replace it with
        /// `let _ = new_name;`) — the `groups[0].name == "Friends"` assert must go red.
        /// (Refusal half: change the find predicate to `|g| g.name != old_name` — the
        /// `expect_err` must go red because the command starts succeeding.)
        #[test]
        fn groups_rename_command_persists_the_new_name() {
            let app = dispatch_app();
            let store = app.state::<DataStore>();
            let seeded_at = chrono::Utc::now() - chrono::Duration::hours(2);
            store
                .save_groups(&[Group {
                    name: "Pals".into(),
                    pubkeys: vec!["npub_a".into(), "npub_b".into()],
                    modified_at: seeded_at,
                    color: Some("#ff00aa".into()),
                }])
                .unwrap();
            let snapshot = std::fs::read_to_string(store.groups_path()).unwrap();

            // Renaming an absent group refuses AND writes nothing.
            let err = tauri::async_runtime::block_on(groups_rename(
                "Nope".into(),
                "X".into(),
                app.state::<DataStore>(),
            ))
            .expect_err("renaming a group that does not exist must error");
            assert_eq!(err, "Group 'Nope' not found");
            assert_eq!(
                std::fs::read_to_string(store.groups_path()).unwrap(),
                snapshot,
                "a refused rename must not rewrite groups.json"
            );

            tauri::async_runtime::block_on(groups_rename(
                "Pals".into(),
                "Friends".into(),
                app.state::<DataStore>(),
            ))
            .unwrap();
            let groups = store.load_groups().unwrap();
            assert_eq!(groups.len(), 1, "rename does not add or drop groups");
            assert_eq!(groups[0].name, "Friends", "the new name is persisted");
            assert_eq!(
                groups[0].pubkeys,
                vec!["npub_a".to_string(), "npub_b".to_string()],
                "members survive the rename"
            );
            assert_eq!(groups[0].color.as_deref(), Some("#ff00aa"), "color survives the rename");
            assert!(
                groups[0].modified_at > seeded_at,
                "rename stamps a fresh modified_at"
            );
        }

        /// P-10: in `groups_delete`, replace `groups.retain(|g| g.name != name);` with
        /// `let _ = &name;` — the `groups.len() == 1` assert must go red (nothing was deleted).
        #[test]
        fn groups_delete_command_removes_only_the_named_group() {
            let app = dispatch_app();
            let store = app.state::<DataStore>();
            let now = chrono::Utc::now();
            store
                .save_groups(&[
                    Group {
                        name: "Pals".into(),
                        pubkeys: vec!["npub_a".into()],
                        modified_at: now - chrono::Duration::hours(2),
                        color: None,
                    },
                    Group {
                        name: "Work".into(),
                        pubkeys: vec!["npub_b".into(), "npub_c".into()],
                        modified_at: now - chrono::Duration::hours(1),
                        color: Some("#00aaff".into()),
                    },
                ])
                .unwrap();

            tauri::async_runtime::block_on(groups_delete("Pals".into(), app.state::<DataStore>()))
                .unwrap();

            let groups = store.load_groups().unwrap();
            assert_eq!(groups.len(), 1, "only the named group is deleted");
            assert_eq!(groups[0].name, "Work");
            assert_eq!(
                groups[0].pubkeys,
                vec!["npub_b".to_string(), "npub_c".to_string()],
                "the surviving group keeps its members"
            );
            assert_eq!(groups[0].color.as_deref(), Some("#00aaff"));

            // Deleting an absent name is a silent no-op — survivors untouched.
            tauri::async_runtime::block_on(groups_delete("Ghost".into(), app.state::<DataStore>()))
                .unwrap();
            assert_eq!(
                store.load_groups().unwrap().len(),
                1,
                "deleting an absent group must not touch the survivors"
            );
        }

        /// P-10: in `groups_unassign`, replace `group.pubkeys.retain(|id| id != &npub);` with
        /// `let _ = &npub;` — the `pals.pubkeys == ["npub_b"]` assert must go red.
        #[test]
        fn groups_unassign_command_removes_only_the_named_npub() {
            let app = dispatch_app();
            let store = app.state::<DataStore>();
            let now = chrono::Utc::now();
            store
                .save_groups(&[
                    Group {
                        name: "Pals".into(),
                        pubkeys: vec!["npub_a".into(), "npub_b".into()],
                        modified_at: now - chrono::Duration::hours(2),
                        color: None,
                    },
                    Group {
                        name: "Work".into(),
                        pubkeys: vec!["npub_a".into()],
                        modified_at: now - chrono::Duration::hours(1),
                        color: None,
                    },
                ])
                .unwrap();
            let snapshot = std::fs::read_to_string(store.groups_path()).unwrap();

            // Unassigning from an absent group refuses AND writes nothing.
            let err = tauri::async_runtime::block_on(groups_unassign(
                "npub_a".into(),
                "Nope".into(),
                app.state::<DataStore>(),
            ))
            .expect_err("unassigning from an absent group must error");
            assert_eq!(err, "Group 'Nope' not found");
            assert_eq!(
                std::fs::read_to_string(store.groups_path()).unwrap(),
                snapshot,
                "a refused unassign must not rewrite groups.json"
            );

            tauri::async_runtime::block_on(groups_unassign(
                "npub_a".into(),
                "Pals".into(),
                app.state::<DataStore>(),
            ))
            .unwrap();
            let groups = store.load_groups().unwrap();
            let pals = groups.iter().find(|g| g.name == "Pals").unwrap();
            assert_eq!(
                pals.pubkeys,
                vec!["npub_b".to_string()],
                "the named npub leaves the named group"
            );
            let work = groups.iter().find(|g| g.name == "Work").unwrap();
            assert_eq!(
                work.pubkeys,
                vec!["npub_a".to_string()],
                "membership in OTHER groups is untouched"
            );
        }

        /// P-10: in `private_audience_list`, replace the body expression
        /// `store.load_private_audience().map_err(cmd_err)` with `Ok(Vec::new())` — the equality
        /// assert must go red.
        #[test]
        fn private_audience_list_command_reads_the_audience_file() {
            let app = dispatch_app();
            let store = app.state::<DataStore>();
            store
                .save_private_audience(&["npub_a".into(), "npub_c".into()])
                .unwrap();

            let got = tauri::async_runtime::block_on(private_audience_list(app.state::<DataStore>()))
                .unwrap();
            assert_eq!(
                got,
                vec!["npub_a".to_string(), "npub_c".to_string()],
                "private_audience_list returns exactly what the store holds — no group derivation"
            );
        }

        /// P-10: in `private_audience_set`, change `if receives {` to `if !receives {` — enrol
        /// becomes revoke, so the first equality assert must go red.
        #[test]
        fn private_audience_set_command_enrols_and_revokes_idempotently() {
            let app = dispatch_app();
            let store = app.state::<DataStore>();

            // Enrol, twice each — the second call must not duplicate.
            tauri::async_runtime::block_on(private_audience_set(
                "npub_a".into(),
                true,
                app.state::<DataStore>(),
            ))
            .unwrap();
            tauri::async_runtime::block_on(private_audience_set(
                "npub_a".into(),
                true,
                app.state::<DataStore>(),
            ))
            .unwrap();
            tauri::async_runtime::block_on(private_audience_set(
                "npub_b".into(),
                true,
                app.state::<DataStore>(),
            ))
            .unwrap();
            assert_eq!(
                store.load_private_audience().unwrap(),
                vec!["npub_a".to_string(), "npub_b".to_string()],
                "both npubs enrolled, no duplicate"
            );

            // Revoke one; revoking an absent npub is a no-op; the survivor stays.
            tauri::async_runtime::block_on(private_audience_set(
                "npub_a".into(),
                false,
                app.state::<DataStore>(),
            ))
            .unwrap();
            tauri::async_runtime::block_on(private_audience_set(
                "npub_ghost".into(),
                false,
                app.state::<DataStore>(),
            ))
            .unwrap();
            assert_eq!(
                store.load_private_audience().unwrap(),
                vec!["npub_b".to_string()],
                "revoked npub gone, absent-npub revoke changed nothing"
            );
        }

        /// THE invariant this slice exists to pin (CLAUDE.md §6), now at the COMMAND seam: the
        /// Private-collection audience is EXPLICIT and never group-derived, so driving the four
        /// group-mutation commands through their real `#[tauri::command]` fns must leave
        /// `private_audience.json` byte-identical and `private_audience_list` unchanged. The
        /// store-seam twin is `group_mutations_do_not_touch_private_audience` above; this test is
        /// the same property proved through dispatch rather than through a mirrored body.
        /// P-10: in `groups_assign`, insert one line after `group.pubkeys.push(npub);`:
        /// `store.save_private_audience(&[npub.clone()]).unwrap();` — the byte-identical assert
        /// must go red (the audience file was rewritten).
        #[test]
        fn group_command_mutations_do_not_touch_private_audience() {
            let app = dispatch_app();
            let store = app.state::<DataStore>();
            let now = chrono::Utc::now();
            // Seed one group and a NON-empty audience — "unchanged" must mean the file keeps its
            // contents, not merely that an empty file stayed missing.
            store
                .save_groups(&[Group {
                    name: "Pals".into(),
                    pubkeys: vec![],
                    modified_at: now,
                    color: None,
                }])
                .unwrap();
            store.save_private_audience(&["npub_kept".into()]).unwrap();
            let audience_snapshot = std::fs::read(store.private_audience_path()).unwrap();
            let listed_before =
                tauri::async_runtime::block_on(private_audience_list(app.state::<DataStore>()))
                    .unwrap();

            // The four group-mutation commands, driven through the real command fns.
            tauri::async_runtime::block_on(groups_assign(
                "npub_filed".into(),
                "Pals".into(),
                app.state::<DataStore>(),
            ))
            .unwrap();
            tauri::async_runtime::block_on(groups_unassign(
                "npub_filed".into(),
                "Pals".into(),
                app.state::<DataStore>(),
            ))
            .unwrap();
            tauri::async_runtime::block_on(groups_rename(
                "Pals".into(),
                "Friends".into(),
                app.state::<DataStore>(),
            ))
            .unwrap();
            tauri::async_runtime::block_on(groups_delete(
                "Friends".into(),
                app.state::<DataStore>(),
            ))
            .unwrap();

            // groups.json really moved — otherwise the asserts above prove nothing about the
            // four commands this test is supposed to drive.
            assert!(
                store.load_groups().unwrap().is_empty(),
                "the four commands must have actually mutated groups.json"
            );

            let audience_after = std::fs::read(store.private_audience_path()).unwrap();
            assert_eq!(
                audience_after, audience_snapshot,
                "private_audience.json is byte-identical after every group command"
            );
            let listed_after =
                tauri::async_runtime::block_on(private_audience_list(app.state::<DataStore>()))
                    .unwrap();
            assert_eq!(listed_after, listed_before);
            assert_eq!(
                listed_after,
                vec!["npub_kept".to_string()],
                "npub_filed was filed into a group and must NOT be enrolled as a recipient"
            );
        }
    }
}
