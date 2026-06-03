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
    let group = Group { name, pubkeys: vec![] };
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
    hb_id: String,
    group_name: String,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let mut groups = store.load_groups().map_err(cmd_err)?;
    let group = groups
        .iter_mut()
        .find(|g| g.name == group_name)
        .ok_or_else(|| format!("Group '{group_name}' not found"))?;
    if !group.pubkeys.contains(&hb_id) {
        group.pubkeys.push(hb_id);
    }
    store.save_groups(&groups).map_err(cmd_err)
}

#[tauri::command]
pub async fn groups_unassign(
    hb_id: String,
    group_name: String,
    store: State<'_, DataStore>,
) -> CmdResult<()> {
    let mut groups = store.load_groups().map_err(cmd_err)?;
    let group = groups
        .iter_mut()
        .find(|g| g.name == group_name)
        .ok_or_else(|| format!("Group '{group_name}' not found"))?;
    group.pubkeys.retain(|id| id != &hb_id);
    store.save_groups(&groups).map_err(cmd_err)
}
