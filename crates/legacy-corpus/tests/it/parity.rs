use photocraft_doc::{LayerContent, LayerId};
use photocraft_engine::plugin_cmds::{preview_factor, scale_for_preview};
use photocraft_engine::{EngineError, Session, prefs, preset_store, smart_cmds};
use photocraft_legacy_corpus::{
    ABSENT_ID, ACTION_NAME, COMMANDS_ID, FILL_ID, GENERATOR, LAYER_ID, LegacyPath, MANIFEST_ID, PLUGIN_FOLDER, PREVIEW_ID, PROBE_CANVAS, PROBE_ID,
    UPPER_LAYER_ID,
};
use photocraft_plugins::registry;
use serde_json::{Value, json};

use crate::{corpus, document, install, layer_samples, only_layer, original, samples8, session, smart_cache};

fn listed(id: &str) -> Value {
    let list = Session::new().execute("plugin.list", json!({})).unwrap();
    list["plugins"].as_array().unwrap().iter().find(|p| p["id"] == id).unwrap().clone()
}

fn filled() -> Vec<Vec<u8>> {
    vec![vec![64, 64, 64, 64]; 8]
}

fn gray_filled() -> Vec<Vec<u8>> {
    vec![vec![64]; 8]
}

fn smart_object(s: &Session) -> photocraft_doc::SmartObject {
    let LayerContent::Smart(object) = &only_layer(&s.active().unwrap().doc).content else { panic!("not a smart object") };
    object.clone()
}

fn check(kind: LegacyPath) {
    match kind {
        LegacyPath::Abi1Module => {
            fill_module();
            probe_module();
        }
        LegacyPath::Manifest => manifest_module(),
        LegacyPath::TargetChannel => channel_target(),
        LegacyPath::LockTransparency => lock_transparency(),
        LegacyPath::LockPixels => lock_pixels(),
        LegacyPath::SelectionMask => selection_mask(),
        LegacyPath::MaskTarget => mask_target(),
        LegacyPath::QuickMaskTarget => quick_mask_target(),
        LegacyPath::FilterMaskTarget => filter_mask_target(),
        LegacyPath::LayerParam => layer_param(),
        LegacyPath::SmartFilterApply => smart_filter_apply(),
        LegacyPath::SmartFilterRecord => smart_filter_record(),
        LegacyPath::ActionStep => action_step(),
        LegacyPath::PluginCommands => plugin_commands(),
        LegacyPath::PreviewScaling => preview_scaling(),
        LegacyPath::FolderPreference => folder_preference(),
        LegacyPath::LegacyPreferences => legacy_preferences(),
        LegacyPath::MissingPlugin => missing_plugin(),
    }
}

#[test]
fn every_legacy_path_passes_its_parity_test() {
    let failed: Vec<&str> = LegacyPath::ALL.into_iter().filter(|kind| std::panic::catch_unwind(|| check(*kind)).is_err()).map(LegacyPath::name).collect();
    assert_eq!(failed, Vec::<&str>::new());
    assert_eq!(corpus().unread(), Vec::<String>::new(), "every committed member is read by a parity test");
}

#[test]
fn documents_carry_the_pinned_generator() {
    let manifest = photocraft_format::read_manifest(&corpus().bytes("locked-document", "document.pcraft").unwrap()).unwrap();
    assert_eq!(manifest.generator, GENERATOR);
}

fn fill_module() {
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

fn probe_module() {
    install("probe-module", "probe.wasm");
    let mut s = session("probe-module");
    s.execute("plugin.run", json!({"id": PROBE_ID, "level": 2.5, "flag": true, "mode": "b"})).unwrap();
    let layer = only_layer(&s.active().unwrap().doc);
    let samples = layer.surface().unwrap().read_region(PROBE_CANVAS);
    let stream: Vec<f32> = samples.iter().enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, v)| *v).collect();
    let header: Vec<u32> = stream[..5].iter().map(|v| *v as u32).collect();
    let bytes: Vec<u8> = stream[5..5 + header[4] as usize].iter().map(|v| *v as u8).collect();
    assert_eq!(&header[..4], [64, 4, 4, 196896]);
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        r#"{"_image":{"alpha":true,"canvasHeight":4,"canvasWidth":64,"channels":4,"depth":32,"height":4,"mode":"rgb","overlap":0,"width":64,"x":0,"y":0},"count":3,"flag":true,"level":1.0,"mode":"b"}"#
    );
}

fn manifest_module() {
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

fn channel_target() {
    install("fill-module", "fill.wasm");
    let mut s = session("channel-document");
    s.execute("plugin.run", json!({"id": FILL_ID, "target": {"channel": 0}})).unwrap();
    assert_eq!(samples8(&s.active().unwrap().doc.channels[0].surface), gray_filled());
    assert_eq!(layer_samples(&s), original());
}

fn lock_transparency() {
    install("fill-module", "fill.wasm");
    let mut s = session("locked-document");
    s.execute("plugin.run", json!({"id": FILL_ID})).unwrap();
    let expected: Vec<Vec<u8>> = [255, 128, 64, 16, 255, 255, 200, 32].iter().map(|a| vec![64, 64, 64, *a]).collect();
    assert_eq!(layer_samples(&s), expected);
}

fn lock_pixels() {
    install("fill-module", "fill.wasm");
    let mut s = session("pixels-locked-document");
    let steps = s.active().unwrap().history.past_len();
    let refused = s.execute("plugin.run", json!({"id": FILL_ID})).unwrap_err().to_string();
    assert_eq!(refused, "Could not complete your request because the layer \"Corpus Pixels Locked Layer\" is locked");
    assert_eq!((layer_samples(&s), s.active().unwrap().history.past_len()), (original(), steps));
}

fn selection_mask() {
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

fn mask_target() {
    install("fill-module", "fill.wasm");
    let mut s = session("mask-document");
    s.execute("plugin.run", json!({"id": FILL_ID, "target": "mask"})).unwrap();
    let doc = &s.active().unwrap().doc;
    assert_eq!(samples8(&only_layer(doc).mask.as_ref().unwrap().surface), gray_filled());
    assert_eq!(layer_samples(&s), original());
}

fn quick_mask_target() {
    install("fill-module", "fill.wasm");
    let mut s = session("quick-mask-document");
    s.execute("plugin.run", json!({"id": FILL_ID, "target": "quickMask"})).unwrap();
    assert_eq!(samples8(&s.active().unwrap().doc.quick_mask.as_ref().unwrap().surface), gray_filled());
    assert_eq!(layer_samples(&s), original());
}

fn filter_mask_target() {
    install("fill-module", "fill.wasm");
    let mut s = session("filter-mask-document");
    s.execute("plugin.run", json!({"id": FILL_ID, "target": "filterMask"})).unwrap();
    let object = smart_object(&s);
    assert_eq!(samples8(&object.filter_mask.as_ref().unwrap().surface), gray_filled());
    assert_eq!(object.smart_filters.len(), 1);
}

fn layer_param() {
    install("fill-module", "fill.wasm");
    let mut s = session("two-layer-document");
    assert_eq!(s.active().unwrap().active_layer, Some(LayerId(UPPER_LAYER_ID)));
    assert_eq!(s.execute("plugin.run", json!({"id": FILL_ID, "layer": LAYER_ID})).unwrap(), json!({"layer": LAYER_ID, "plugin": FILL_ID}));
    let doc = &s.active().unwrap().doc;
    let pixels = |id: u64| samples8(doc.layer(LayerId(id)).unwrap().surface().unwrap());
    assert_eq!((pixels(LAYER_ID), pixels(UPPER_LAYER_ID)), (filled(), original()));
}

fn smart_filter_apply() {
    install("fill-module", "fill.wasm");
    let mut s = session("smart-apply-document");
    s.execute("plugin.run", json!({"id": FILL_ID})).unwrap();
    let object = smart_object(&s);
    assert_eq!(object.smart_filters.len(), 1);
    assert_eq!((object.smart_filters[0].command.as_str(), &object.smart_filters[0].params), ("plugin.run", &json!({"id": FILL_ID, "params": {}})));
    assert_eq!(samples8(&object.filter_mask.as_ref().unwrap().surface), vec![vec![255], vec![128], vec![0], vec![0], vec![255], vec![128], vec![0], vec![0]]);
    assert_eq!(
        samples8(object.cache.as_ref().unwrap()),
        vec![
            vec![64, 64, 64, 64],
            vec![28, 154, 48, 96],
            vec![40, 10, 200, 64],
            vec![250, 250, 250, 16],
            vec![64, 64, 64, 64],
            vec![115, 115, 115, 159],
            vec![90, 60, 30, 200],
            vec![30, 60, 90, 32],
        ]
    );
}

fn smart_filter_record() {
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

fn action_step() {
    install("fill-module", "fill.wasm");
    let dir = tempfile::tempdir().unwrap();
    corpus().copy_member("action", dir.path()).unwrap();
    let opened = preset_store::open_dir(dir.path());
    assert_eq!(opened.warnings, Vec::<String>::new());
    let mut s = session("channel-document");
    s.actions.list = opened.actions;
    let played = s.execute("actions.play", json!({"action": ACTION_NAME})).unwrap();
    assert_eq!((&played["action"], &played["ran"], played.get("failed")), (&json!(ACTION_NAME), &json!(1), None));
    assert_eq!(layer_samples(&s), filled());
}

fn plugin_commands() {
    let dir = tempfile::tempdir().unwrap();
    corpus().copy_member("commands-module", dir.path()).unwrap();
    let path = dir.path().join("commands.wasm");
    let path_text = path.to_string_lossy().into_owned();
    let mut s = Session::new();
    let installed = s.execute("plugin.install", json!({"path": path_text})).unwrap();
    assert_eq!((&installed["id"], &installed["source"]), (&json!(COMMANDS_ID), &json!(path_text)));
    let refused = s.execute("plugin.install", json!({"path": path_text, "replace": false})).unwrap_err().to_string();
    assert_eq!(refused, format!("invalid parameters for `plugin.install`: a plug-in with id {COMMANDS_ID:?} is already installed"));
    let data = photocraft_paint::tile::b64_encode(&std::fs::read(&path).unwrap());
    assert_eq!(s.execute("plugin.install", json!({"data": data})).unwrap()["source"], Value::Null);
    assert_eq!(s.execute("plugin.remove", json!({"id": COMMANDS_ID})).unwrap(), json!({"removed": COMMANDS_ID}));
    assert_eq!(s.execute("plugin.remove", json!({"id": COMMANDS_ID})).unwrap_err().to_string(), format!("no plug-in {COMMANDS_ID:?} is installed"));
    let folder = dir.path().to_string_lossy().into_owned();
    assert_eq!(s.execute("plugin.reload", json!({"path": folder})).unwrap(), json!({"folder": folder, "loaded": [COMMANDS_ID], "failed": []}));
    assert!(registry::get(COMMANDS_ID).is_some());
}

fn preview_scaling() {
    install("preview-module", "preview.wasm");
    let mut flat = json!({"id": PREVIEW_ID, "radius": 12, "size": 10, "amount": 40, "channel": 1});
    scale_for_preview(&mut flat, 4);
    assert_eq!(flat, json!({"id": PREVIEW_ID, "radius": 3.0, "size": 3, "amount": 40, "channel": 1}));
    let mut nested = json!({"id": PREVIEW_ID, "params": {"amount": 40}});
    scale_for_preview(&mut nested, 2);
    assert_eq!(nested, json!({"id": PREVIEW_ID, "params": {"radius": 6.0, "size": 5, "amount": 40}}));
    assert_eq!(preview_factor(&json!({"id": PREVIEW_ID, "size": 10}), 4), 4);
    assert_eq!(preview_factor(&json!({"id": PREVIEW_ID, "size": 5}), 4), 2);
    assert_eq!(preview_factor(&json!({"id": PREVIEW_ID, "size": 3}), 4), 1);
}

fn folder_preference() {
    let dir = tempfile::tempdir().unwrap();
    corpus().copy_member("plugin-folder", dir.path()).unwrap();
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

fn legacy_preferences() {
    let dir = tempfile::tempdir().unwrap();
    corpus().copy_member("legacy-preferences", dir.path()).unwrap();
    let saved = prefs::read_saved(Some(dir.path()));
    let plug_ins = &saved.prefs.plug_ins;
    assert_eq!((plug_ins.use_additional_plugins_folder, plug_ins.additional_plugins_folder.as_str()), (true, PLUGIN_FOLDER));
    assert!(!dir.path().join("config.toml").exists());
}

fn missing_plugin() {
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
