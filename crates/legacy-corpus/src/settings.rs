use photocraft_engine::actions_cmds::{Action, ActionList, ActionStep, ActionsFile};
use photocraft_engine::plugin_cmds;
use photocraft_engine::prefs::PlugIns;
use serde::Serialize;

use crate::Error;
use crate::modules::FILL_ID;

pub const ACTION_NAME: &str = "Corpus Fill";
pub const PLUGIN_FOLDER: &str = "Plug-ins";

pub fn actions() -> Result<Vec<u8>, Error> {
    let step = ActionStep::new(plugin_cmds::RUN, serde_json::json!({"id": FILL_ID}));
    let list = ActionList::from_file(ActionsFile::new(vec![Action::new(ACTION_NAME, vec![step])], Vec::new()));
    list.to_file().to_json().map(String::into_bytes).map_err(|e| Error::Settings(e.to_string()))
}

#[derive(Serialize)]
struct Config {
    schema_version: u32,
    #[serde(rename = "plugIns")]
    plug_ins: PlugIns,
}

pub fn plugin_folder() -> Result<Vec<u8>, Error> {
    let plug_ins = PlugIns { use_additional_plugins_folder: true, additional_plugins_folder: PLUGIN_FOLDER.into(), ..PlugIns::default() };
    toml::to_string(&Config { schema_version: 1, plug_ins }).map(String::into_bytes).map_err(|e| Error::Settings(e.to_string()))
}
