use craft::settings::SettingsStore;
use photocraft_engine::actions_cmds::{Action, ActionList, ActionStep, ActionsFile};
use photocraft_engine::prefs::{SavedPrefs, store_options};
use photocraft_engine::{Session, plugin_cmds};

use crate::Error;
use crate::modules::FILL_ID;

pub const ACTION_NAME: &str = "Corpus Fill";
pub const PLUGIN_FOLDER: &str = "Plug-ins";
pub const CONFIG_FILE: &str = "config.toml";

fn settings_error(e: impl std::fmt::Display) -> Error {
    Error::Settings(e.to_string())
}

pub fn actions() -> Result<Vec<u8>, Error> {
    let step = ActionStep::new(plugin_cmds::RUN, serde_json::json!({"id": FILL_ID}));
    let list = ActionList::from_file(ActionsFile::new(vec![Action::new(ACTION_NAME, vec![step])], Vec::new()));
    list.to_file().to_json().map(String::into_bytes).map_err(settings_error)
}

fn with_folder(saved: &mut SavedPrefs) {
    saved.prefs.plug_ins.use_additional_plugins_folder = true;
    saved.prefs.plug_ins.additional_plugins_folder = PLUGIN_FOLDER.into();
}

pub fn plugin_folder() -> Result<Vec<u8>, Error> {
    let dir = tempfile::tempdir().map_err(settings_error)?;
    let (mut store, _) = SettingsStore::<SavedPrefs>::open_with(store_options(Some(dir.path())));
    let mut saved = store.get().clone();
    with_folder(&mut saved);
    store.set(saved);
    store.flush().map_err(settings_error)?;
    std::fs::read(dir.path().join(CONFIG_FILE)).map_err(settings_error)
}

pub fn legacy_preferences() -> Result<Vec<u8>, Error> {
    let mut full = Session::new().prefs_value();
    let mut saved = SavedPrefs::default();
    with_folder(&mut saved);
    full["plugIns"] = serde_json::to_value(&saved.prefs.plug_ins).map_err(settings_error)?;
    let kept: serde_json::Map<String, serde_json::Value> =
        ["version", "plugIns"].iter().filter_map(|key| full.get(*key).map(|value| ((*key).to_owned(), value.clone()))).collect();
    serde_json::to_vec_pretty(&kept).map_err(settings_error)
}
