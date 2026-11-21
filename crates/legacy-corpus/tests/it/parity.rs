use photocraft_doc::LayerContent;
use photocraft_engine::{EngineError, Session, prefs, preset_store, smart_cmds};
use photocraft_legacy_corpus::{ABSENT_ID, ACTION_NAME, FILL_ID, MANIFEST_ID, PLUGIN_FOLDER};
use photocraft_plugins::registry;
use serde_json::{Value, json};

use crate::{copy_member, document, install, layer_samples, only_layer, original, samples8, session, smart_cache};

fn listed(id: &str) -> Value {
    let list = Session::new().execute("plugin.list", json!({})).unwrap();
    list["plugins"].as_array().unwrap().iter().find(|p| p["id"] == id).unwrap().clone()
}

fn filled() -> Vec<Vec<u8>> {
    vec![vec![64, 64, 64, 64]; 8]
}

#[test]
fn fill_module_sets_every_sample_and_is_one_undo_step() {
    install("fill-module", "fill.wasm");
    let entry = listed(FILL_ID);
    assert_eq!(
        (&entry["name"], &entry["version"], &entry["params"], &entry["area"]),
        (&json!("Corpus Fill"), &json!("1.0.0"), &json!("{}"), &json!("content"))
    );
    let mut s = session("channel-document");
    assert_eq!(s.execute("plugin.run", json!({"id": FILL_ID})).unwrap(), json!({"layer": 2, "plugin": FILL_ID}));
    assert_eq!(layer_samples(&s), filled());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(layer_samples(&s), original());
}

#[test]
fn manifest_module_declares_every_parameter_type() {
    install("manifest-module", "manifest.wasm");
    let entry = listed(MANIFEST_ID);
    assert_eq!(entry["params"], json!(r#"{"edge":"wrap|clamp|mirror","invert":bool=false,"radius":0..50=4,"steps":int=3}"#));
    assert_eq!(entry["paramsSchema"]["radius"], json!({"type": "number", "min": 0.0, "max": 50.0, "default": 4.0, "unit": "px"}));
    assert_eq!((&entry["overlap"], &entry["area"], &entry["version"]), (&json!(2), &json!("canvas"), &json!("2.1.0")));
    let mut s = session("channel-document");
    assert!(matches!(s.execute("plugin.run", json!({"id": MANIFEST_ID, "steps": "three"})), Err(EngineError::BadParams { .. })));
    s.execute("plugin.run", json!({"id": MANIFEST_ID, "radius": 8, "edge": "mirror"})).unwrap();
    assert_eq!(layer_samples(&s), original());
}

#[test]
fn channel_target_fills_the_alpha_channel_and_leaves_the_layer() {
    install("fill-module", "fill.wasm");
    let mut s = session("channel-document");
    s.execute("plugin.run", json!({"id": FILL_ID, "target": {"channel": 0}})).unwrap();
    let doc = &s.active().unwrap().doc;
    assert_eq!(samples8(&doc.channels[0].surface), vec![vec![64]; 8]);
    assert_eq!(layer_samples(&s), original());
}

#[test]
fn lock_transparency_keeps_every_alpha() {
    install("fill-module", "fill.wasm");
    let mut s = session("locked-document");
    s.execute("plugin.run", json!({"id": FILL_ID})).unwrap();
    let expected: Vec<Vec<u8>> = [255, 128, 64, 16, 255, 255, 200, 32].iter().map(|a| vec![64, 64, 64, *a]).collect();
    assert_eq!(layer_samples(&s), expected);
}

#[test]
fn the_saved_selection_blends_the_fill() {
    install("fill-module", "fill.wasm");
    let mut s = session("selection-document");
    s.execute("plugin.run", json!({"id": FILL_ID})).unwrap();
    assert_eq!(
        layer_samples(&s),
        vec![
            vec![64, 64, 64, 64],
            vec![37, 132, 52, 96],
            vec![40, 10, 200, 64],
            vec![250, 250, 250, 16],
            vec![64, 64, 64, 64],
            vec![96, 96, 96, 159],
            vec![90, 60, 30, 200],
            vec![30, 60, 90, 32],
        ]
    );
}

#[test]
fn the_smart_filter_record_reopens_and_renders_with_the_plugin() {
    install("fill-module", "fill.wasm");
    let mut doc = document("smart-filter-document");
    let id = only_layer(&doc).id;
    let LayerContent::Smart(object) = &only_layer(&doc).content else { panic!("not a smart object") };
    assert_eq!(object.smart_filters.len(), 1);
    assert_eq!((object.smart_filters[0].command.as_str(), &object.smart_filters[0].params), ("plugin.run", &json!({"id": FILL_ID, "params": {}})));
    assert_eq!(smart_cache(&doc), filled());
    smart_cmds::refresh(&mut doc, id).unwrap();
    assert_eq!(smart_cache(&doc), filled());
}

#[test]
fn the_recorded_action_plays_the_plugin_step() {
    install("fill-module", "fill.wasm");
    let dir = tempfile::tempdir().unwrap();
    copy_member("action", dir.path());
    let opened = preset_store::open_dir(dir.path());
    assert_eq!(opened.warnings, Vec::<String>::new());
    let mut s = session("channel-document");
    s.actions.list = opened.actions;
    let played = s.execute("actions.play", json!({"action": ACTION_NAME})).unwrap();
    assert_eq!((&played["action"], &played["ran"], played.get("failed")), (&json!(ACTION_NAME), &json!(1), None));
    assert_eq!(layer_samples(&s), filled());
}

#[test]
fn the_folder_preference_loads_the_plugins_folder() {
    let dir = tempfile::tempdir().unwrap();
    copy_member("plugin-folder", dir.path());
    let mut saved = prefs::read_saved(Some(dir.path()));
    let plug_ins = &saved.prefs.plug_ins;
    assert_eq!((plug_ins.use_additional_plugins_folder, plug_ins.additional_plugins_folder.as_str()), (true, PLUGIN_FOLDER));
    let folder = dir.path().join(PLUGIN_FOLDER);
    saved.prefs.plug_ins.additional_plugins_folder = folder.to_string_lossy().into_owned();
    let mut s = Session::new();
    s.set_saved_prefs(saved);
    let report = s.execute("plugin.list", json!({})).unwrap()["folder"].clone();
    assert_eq!(report["loaded"], json!([FILL_ID]));
    let failed = report["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["file"], json!(folder.join("not-a-module.wasm").to_string_lossy()));
    assert!(registry::get(FILL_ID).is_some());
}

#[test]
fn a_record_for_a_missing_plugin_survives_and_renders_unfiltered() {
    let mut doc = document("missing-plugin-document");
    assert!(registry::get(ABSENT_ID).is_none());
    let id = only_layer(&doc).id;
    assert_eq!(smart_cache(&doc), original());
    smart_cmds::refresh(&mut doc, id).unwrap();
    assert_eq!(smart_cache(&doc), original());
    let resaved = photocraft_format::save_to_bytes(&doc, &photocraft_format::SaveOptions::default()).unwrap();
    let reopened = photocraft_format::load_from_bytes(&resaved).unwrap();
    let LayerContent::Smart(object) = &only_layer(&reopened).content else { panic!("not a smart object") };
    assert_eq!(object.smart_filters[0].params, json!({"id": ABSENT_ID, "params": {}}));
    let mut s = session("missing-plugin-document");
    let refused = s.execute("plugin.run", json!({"id": ABSENT_ID})).unwrap_err().to_string();
    assert_eq!(refused, format!("no plug-in {ABSENT_ID:?} is installed"));
}
