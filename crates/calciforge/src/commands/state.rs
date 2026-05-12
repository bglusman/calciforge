use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::calciforge_config_home;

/// Default state directory: `~/.config/calciforge/state/`.
pub(super) fn default_state_dir() -> PathBuf {
    calciforge_config_home(None).join("state")
}

/// Load persisted active-agent selections from a given state directory.
/// Returns an empty map if the file doesn't exist or can't be parsed.
pub(super) fn load_active_agents_from(state_dir: &Path) -> HashMap<String, String> {
    let path = state_dir.join("active-agents.json");
    read_json_map(&path)
}

/// Load persisted active gateway model selections from a given state directory.
/// Returns an empty map if the file doesn't exist or can't be parsed.
pub(super) fn load_active_models_from(state_dir: &Path) -> HashMap<String, String> {
    let path = state_dir.join("active-models.json");
    read_json_map(&path)
}

/// Load persisted active downstream session selections.
/// Returns an empty map if the file doesn't exist or can't be parsed.
pub(super) fn load_active_sessions_from(
    state_dir: &Path,
) -> HashMap<String, HashMap<String, String>> {
    let path = state_dir.join("active-agent-sessions.json");
    read_json_map(&path)
}

/// Persist the active-agent map to a given state directory.
pub(super) fn save_active_agents_to(state_dir: &Path, map: &HashMap<String, String>) {
    let path = state_dir.join("active-agents.json");
    write_json_map(&path, map);
}

/// Persist the active gateway model map to a given state directory.
pub(super) fn save_active_models_to(state_dir: &Path, map: &HashMap<String, String>) {
    let path = state_dir.join("active-models.json");
    write_json_map(&path, map);
}

/// Persist the active downstream session selections to a given state directory.
pub(super) fn save_active_sessions_to(
    state_dir: &Path,
    map: &HashMap<String, HashMap<String, String>>,
) {
    let path = state_dir.join("active-agent-sessions.json");
    write_json_map(&path, map);
}

fn read_json_map<T>(path: &Path) -> T
where
    T: serde::de::DeserializeOwned + Default,
{
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => T::default(),
    }
}

fn write_json_map<T>(path: &Path, map: &T)
where
    T: serde::Serialize,
{
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(map) {
        let _ = std::fs::write(path, json);
    }
}
