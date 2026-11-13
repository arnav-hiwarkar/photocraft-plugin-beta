//! The PhotoCraft example plugins (craft-extensions/examples/photocraft-*) through the engine, as the
//! desktop app and the CLI drive them: the Gradient Ramp tool (one undo step per gesture, Escape
//! and revocation mid-gesture roll back, the live overlay), the Levels Scope panel and command,
//! Layer Export through a picked folder, and pause after three failures with Resume.
//!
//! Run against a disposable store with the three release-built packages:
//! `CRAFT_EXT_DIR=$(mktemp -d) PHOTOCRAFT_PLUGIN_EXAMPLES=<dir with the .craftplugin files>
//!  cargo test -p photocraft-engine --test plugin_examples -- --ignored --nocapture`
use craft_ext_core::id::ExtensionId;
use craft_ext_core::permission::Permission;
use photocraft_doc::LayerContent;
use photocraft_engine::Session;
use serde_json::{Value, json};

const GRADIENT: &str = "io.github.arnav-hiwarkar.photocraft-gradient-ramp";
const LEVELS: &str = "io.github.arnav-hiwarkar.photocraft-levels-scope";
const EXPORT: &str = "io.github.arnav-hiwarkar.photocraft-layer-export";

fn install(dir: &std::path::Path, file: &str, id: &str, grants: &[&str]) {
    let m = photocraft_plugins::store::manager().unwrap();
    let (installed, _, _, _) = m.install_file(&dir.join(file)).unwrap();
    let id = ExtensionId::new(id).unwrap();
    m.store.set_active(&m.app.clone().unwrap(), &id, installed.version).unwrap();
    for g in grants {
        m.grant(&id, Permission::parse(g).unwrap()).unwrap();
    }
    for c in &installed.manifest.contributions {
        m.enable(&id, &c.id).unwrap();
    }
}

fn pixel(s: &Session, layer: photocraft_doc::LayerId, x: i32, y: i32) -> Vec<f32> {
    let d = s.active().unwrap();
    let LayerContent::Raster(surface) = &d.doc.layer(layer).unwrap().content else { panic!("pixel layer") };
    surface.read_region(photocraft_geom::Rect::from_xywh(x, y, 1, 1))
}

#[test]
#[ignore = "requires the three example packages and a disposable CRAFT_EXT_DIR"]
fn photocraft_example_plugins_end_to_end() {
    assert!(std::env::var_os("CRAFT_EXT_DIR").is_some(), "use a disposable CRAFT_EXT_DIR");
    let dir = std::path::PathBuf::from(std::env::var("PHOTOCRAFT_PLUGIN_EXAMPLES").expect("PHOTOCRAFT_PLUGIN_EXAMPLES"));
    install(&dir, "photocraft-gradient-tool.craftplugin", GRADIENT, &["document.edit:document"]);
    install(&dir, "photocraft-levels-scope.craftplugin", LEVELS, &["document.read:document", "document.edit:document", "app.events"]);
    install(&dir, "photocraft-layer-export.craftplugin", EXPORT, &["document.read:document", "files.write:picked"]);
    gradient_tool();
    levels_scope();
    layer_export();
    pause_and_resume();
}

fn gradient_tool() {
    let tool = format!("plugin.session.{GRADIENT}/gradient");
    let mut s = Session::new();
    s.execute("file.new", json!({"width":32,"height":16,"background":"#808080"})).unwrap();
    let before = s.active().unwrap().clone();
    let n = before.history.past_len();
    // A whole gesture from the CLI/automation: begin, updates, commit.
    s.execute(&tool, json!({"points":[[0,8],[10,8],[20,8],[32,8]]})).unwrap();
    let d = s.active().unwrap();
    assert_eq!(d.history.past_len(), n + 1, "one gesture, one undo step");
    assert_eq!(d.history.undo_label(), Some("Gradient Ramp · Gradient Ramp"));
    let layer = d.active_layer.unwrap();
    assert_eq!(d.doc.layer(layer).unwrap().name, "Gradient");
    let (left, right) = (pixel(&s, layer, 0, 8), pixel(&s, layer, 31, 8));
    // Defaults: deep blue to warm orange, left to right.
    assert!(left[2] > left[0] && right[0] > right[2], "{left:?} → {right:?}");
    assert!(s.plugin_overlays().is_empty(), "the guide ends with the gesture");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc, before.doc);

    // Cancelled gestures leave nothing: no layer, no history step.
    s.execute(&tool, json!({"points":[[0,0],[30,10]],"cancel":true})).unwrap();
    assert_eq!(s.active().unwrap().doc, before.doc);
    assert_eq!(s.active().unwrap().history.past_len(), n);

    // The desktop path: select the tool, then pointer events. The guide shows while dragging.
    s.execute(&tool, json!({})).unwrap();
    assert_eq!(s.plugin_tool.as_deref(), Some(tool.as_str()));
    s.plugin_tool_event("begin", [2.0, 2.0], json!({"modifiers":{"shift":true},"zoom":2.0})).unwrap();
    s.plugin_tool_event("update", [20.0, 5.0], json!({"modifiers":{"shift":true},"zoom":2.0})).unwrap();
    assert!(!s.plugin_overlays().is_empty(), "the live guide");
    // Escape rolls the gesture back; the tool stays selected, a second Escape puts it down.
    s.plugin_tool_escape();
    assert!(!s.plugin_gesture_active());
    assert!(s.plugin_overlays().is_empty());
    assert_eq!(s.active().unwrap().doc, before.doc);
    assert!(s.plugin_tool.is_some());
    s.plugin_tool_escape();
    assert!(s.plugin_tool.is_none());

    // Revocation mid-gesture: the next pump rolls everything back and puts the tool down.
    s.execute(&tool, json!({})).unwrap();
    s.plugin_tool_event("begin", [0.0, 0.0], json!({})).unwrap();
    s.plugin_tool_event("update", [16.0, 0.0], json!({})).unwrap();
    let m = photocraft_plugins::store::manager().unwrap();
    let id = ExtensionId::new(GRADIENT).unwrap();
    let edit = Permission::parse("document.edit:document").unwrap();
    m.store.revoke(&m.app.clone().unwrap(), &id, &edit).unwrap();
    s.pump_plugins();
    assert!(!s.plugin_gesture_active());
    assert_eq!(s.active().unwrap().doc, before.doc);
    assert_eq!(s.active().unwrap().history.past_len(), n);
    assert!(s.plugin_tool.is_none());
    m.grant(&id, edit).unwrap();
    m.enable(&id, "gradient").unwrap();
    // Granted again, the tool works again.
    s.execute(&tool, json!({"points":[[0,0],[32,0]]})).unwrap();
    assert_eq!(s.active().unwrap().history.past_len(), n + 1);
}

fn levels_scope() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width":16,"height":16,"background":"#406080"})).unwrap();
    let n = s.active().unwrap().history.past_len();
    s.execute(&format!("plugin.session.{LEVELS}/auto-levels-layer"), json!({})).unwrap();
    let d = s.active().unwrap();
    assert_eq!(d.history.past_len(), n + 1);
    let top = d.doc.layers.last().unwrap();
    assert!(matches!(top.content, LayerContent::Adjustment(_)), "a Levels layer: {:?}", top.content.kind_name());
    // The panel: a level-1.1 tree with tabs and the histogram canvas, drawn by the shared renderer.
    s.pump_plugins();
    // Panels start closed; open it from its menu entry as a user does.
    s.execute(&format!("plugin.session.{LEVELS}/levels-scope"), json!({})).unwrap();
    let rt = s.plugin_sessions.as_ref().unwrap();
    let panel = rt.surfaces.get(&format!("{LEVELS}/levels-scope")).expect("the Levels Scope panel");
    assert!(panel.open);
    let tree = serde_json::to_string(&panel.tree).unwrap();
    assert!(tree.contains("\"tabs\"") && tree.contains("\"canvas\""), "{tree}");
    // Its button through the UI event route: another Levels layer, one more step.
    let event = json!({"surface": panel.key, "rev": panel.rev, "widget": "add-layer", "value": Value::Null});
    s.execute("plugin.uiEvent", event).unwrap();
    assert_eq!(s.active().unwrap().history.past_len(), n + 2);
}

fn layer_export() {
    let out = tempfile::tempdir().unwrap();
    let mut s = Session::new();
    s.execute("file.new", json!({"width":8,"height":8,"background":"#ff0000"})).unwrap();
    s.execute("layer.new.layer", json!({"name":"Top"})).unwrap();
    s.plugin_picks.answers.push_back(out.path().to_path_buf());
    let before = s.active().unwrap().clone();
    s.execute(&format!("plugin.session.{EXPORT}/export-all"), json!({})).unwrap();
    let mut files: Vec<String> = std::fs::read_dir(out.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    files.sort();
    assert_eq!(files, ["Background.png", "Top.png", "layers.json"], "{files:?}");
    assert!(std::fs::read(out.path().join("Background.png")).unwrap().starts_with(b"\x89PNG"));
    let index: Value = serde_json::from_slice(&std::fs::read(out.path().join("layers.json")).unwrap()).unwrap();
    assert_eq!(index["layers"].as_array().unwrap().len(), 2);
    assert_eq!(s.active().unwrap().doc, before.doc, "exporting changes nothing");
    // A publication failure reaches the CLI caller, and the successful-event UI is not published.
    let blocked = tempfile::tempdir().unwrap();
    std::fs::create_dir(blocked.path().join("Top.png")).unwrap();
    s.plugin_picks.answers.push_back(blocked.path().to_path_buf());
    let error = s.execute(&format!("plugin.session.{EXPORT}/export-all"), json!({})).unwrap_err();
    assert!(error.to_string().contains("0 file(s) already published"), "{error}");
    assert_eq!(s.active().unwrap().doc, before.doc);
    assert!(!blocked.path().join("layers.json").exists());
    let panel = &s.plugin_sessions.as_ref().unwrap().surfaces[&format!("{EXPORT}/export")];
    assert!(!serde_json::to_string(&panel.tree).unwrap().contains("Exported 2 layer(s)"), "a failed commit must not publish success UI");
    // Without an answer the plugin hears "cancelled" and writes nothing; the CLI says why.
    let r = s.execute(&format!("plugin.session.{EXPORT}/export-all"), json!({})).unwrap();
    assert!(r["notifications"].to_string().contains("--pick"), "{r}");
}

fn pause_and_resume() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width":4,"height":4})).unwrap();
    // A synchronous command serves the plugins' `init` events first, so the panel exists.
    let auto = format!("plugin.session.{LEVELS}/auto-levels-layer");
    s.execute(&auto, json!({})).unwrap();
    let key = format!("{LEVELS}/levels-scope");
    s.execute(&format!("plugin.session.{key}"), json!({})).unwrap();
    let send = |s: &mut Session, value: Value| {
        let rev = s.plugin_sessions.as_ref().unwrap().surfaces[&key].rev;
        s.execute("plugin.uiEvent", json!({"surface": key, "rev": rev, "widget": "black", "value": value}))
    };
    // A number field that sends text: the plugin's event fails each time.
    for _ in 0..3 {
        assert!(send(&mut s, json!("not a number")).is_err());
    }
    let paused = s.execute("plugin.paused", json!({})).unwrap();
    assert_eq!(paused[0]["plugin"], LEVELS, "{paused}");
    let err = send(&mut s, json!(12)).unwrap_err();
    assert!(err.to_string().contains("paused"), "{err}");
    s.execute("plugin.resume", json!({"plugin": LEVELS})).unwrap();
    assert!(s.plugins_paused().is_empty());
    // Resume sends `init` again; serve it before using the redrawn panel.
    s.execute(&auto, json!({})).unwrap();
    send(&mut s, json!(12)).unwrap();
    assert!(s.execute("plugin.resume", json!({"plugin": LEVELS})).is_err(), "not paused any more");
}
