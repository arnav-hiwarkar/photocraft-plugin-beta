//! Real-package acceptance test. Run only against an explicitly disposable, prepared store.
use photocraft_engine::Session;
use serde_json::json;

const EFFECT: &str = "plugin.io.github.arnav-hiwarkar.halftone/halftone";

#[test]
#[ignore = "requires installed beta catalogue packages in a disposable CRAFT_EXT_DIR"]
fn installed_beta_filters_resources_and_revocation() {
    assert_eq!(std::env::var("PHOTOCRAFT_PLUGIN_BETA_TEST").as_deref(), Ok("1"));
    assert!(std::env::var_os("CRAFT_EXT_DIR").is_some());
    let presets = photocraft_engine::plugin_filters::dialog_fields(EFFECT).expect("installed filter");
    let presets = presets["__presets"].as_array().unwrap();
    assert_eq!(presets.len(), 2, "film-looks single-effect recipes must be offered");
    for depth in [8, 32] {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":32,"height":32,"depth":depth,"background":"#806040"})).unwrap();
        let before = s.active().unwrap().doc.clone();
        let layer = s.active().unwrap().active_layer.unwrap();
        let pixels = |doc: &photocraft_doc::Document| {
            let photocraft_doc::LayerContent::Raster(surface) = &doc.layer(layer).unwrap().content else { panic!("pixel layer") };
            surface.read_region(doc.bounds())
        };
        let before_pixels = pixels(&before);
        let n = s.active().unwrap().history.past_len();
        s.execute(EFFECT, presets[0]["fields"].clone()).unwrap();
        let d = s.active().unwrap();
        assert_ne!(pixels(&d.doc), before_pixels, "{depth}-bit pixels must change");
        assert_eq!(d.history.past_len(), n + 1);
        assert_eq!(d.history.undo_label(), Some("Halftone · Halftone"));
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc, before);
        s.execute("edit.redo", json!({})).unwrap();
        for bad in [json!({"size":1}), json!({"mono":"yes"}), json!({"unknown":1})] {
            let before = s.active().unwrap().doc.clone();
            assert!(s.execute(EFFECT, bad).is_err());
            assert_eq!(s.active().unwrap().doc, before);
        }
    }
    for mode in ["cmyk", "lab"] {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":4,"height":4,"mode":mode})).unwrap();
        let err = s.execute(EFFECT, json!({})).unwrap_err();
        assert!(err.to_string().contains("This filter works on RGB images"), "{err}");
    }
    // This is the discovery function used by both Color Lookup pickers. Parse the
    // offered file through the same engine Color Lookup path as a picker selection.
    let luts = photocraft_plugins::resources::luts().unwrap();
    let (label, path) = luts.iter().find(|(label, _)| label.contains("Film warm")).expect("film LUT offered");
    assert!(label.contains("film-looks"));
    let mut s = Session::new();
    s.execute("file.new", json!({"width":4,"height":4})).unwrap();
    s.execute("image.adjustments.colorLookup", json!({"file":path})).unwrap();

    session_command();

    let source = photocraft_plugins::store::filters().unwrap().into_iter().next().unwrap();
    let generation = source.grant_generation().unwrap();
    let m = photocraft_plugins::store::manager().unwrap();
    let p = craft_ext_core::permission::Permission::parse("document.read:document").unwrap();
    m.store.grant(&m.app.clone().unwrap(), &source.plugin, p.clone()).unwrap();
    m.store.revoke(&m.app.clone().unwrap(), &source.plugin, &p).unwrap();
    assert!(source.validate(generation).is_err(), "revoked epoch must reject captured invocation");
    let generation = source.grant_generation().unwrap();
    m.store
        .apply_revocations(vec![craft_ext_core::catalogue::Revocation {
            id: Some(source.plugin.clone()),
            versions: None,
            sha256: None,
            vendor: None,
            severity: craft_ext_core::catalogue::Severity::Disable,
            reason: "beta acceptance test".into(),
            date: "2026-11-13".into(),
        }])
        .unwrap();
    assert!(source.validate(generation).is_err());
    let before = s.active().unwrap().doc.clone();
    assert!(s.execute(EFFECT, json!({})).is_err(), "cached command must refuse killed release");
    assert_eq!(s.active().unwrap().doc, before);
    m.store.apply_revocations(vec![]).unwrap();
}

// A tiny real Wasm command exercises CLI/automation's synchronous worker bridge.
fn session_command() {
    use std::fs;
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("craft-plugin.toml"),
        r#"schema = 2
id = "io.test.photocraft-session"
name = "Session acceptance"
version = "1.0.0"
license = "MIT"
source = "https://example.com/session"
summary = "Synthetic headless acceptance fixture"
apps = ["photocraft"]
[[contribution]]
id = "rename"
kind = "command"
contract = "photocraft.session@1.0"
label = "Rename"
module = "session.wasm"
permissions = ["document.edit:document"]
"#,
    )
    .unwrap();
    let wasm = wat::parse_str(
        r#"(module
      (import "craft" "call" (func $call (param i32 i32 i32 i32) (result i64)))
      (memory (export "memory") 32)
      (func (export "craft_abi") (result i32) (i32.const 1))
      (func (export "craft_reset"))
      (func (export "craft_alloc") (param i32) (result i32) (i32.const 4096))
      (data (i32.const 16) "layer.rename")
      (data (i32.const 64) "{\22name\22:\22Plugin renamed\22}")
      (func (export "craft_event") (param $p i32) (param i32) (result i32)
        (if (i32.eq (i32.load8_u (i32.add (local.get $p) (i32.const 10))) (i32.const 99))
          (then (drop (call $call (i32.const 16) (i32.const 12) (i32.const 64) (i32.const 25)))))
        (i32.const 0)))"#,
    )
    .unwrap();
    fs::write(dir.path().join("session.wasm"), wasm).unwrap();
    let bin = std::env::var("PHOTOCRAFT_CRAFT_EXT_BIN").expect("craft-ext binary for packing fixture");
    let package = dir.path().join("session.craftplugin");
    let result = Command::new(bin).arg("pack").arg(dir.path()).arg("--out").arg(&package).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let m = photocraft_plugins::store::manager().unwrap();
    let (installed, _, _, _) = m.install_file(&package).unwrap();
    let id = craft_ext_core::id::ExtensionId::new("io.test.photocraft-session").unwrap();
    m.store.set_active(&m.app.clone().unwrap(), &id, installed.version).unwrap();
    m.grant(&id, craft_ext_core::permission::Permission::parse("document.edit:document").unwrap()).unwrap();
    m.enable(&id, "rename").unwrap();
    // Dynamic command discovery uses the same two-second cadence as desktop menus.
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let mut s = Session::new();
    s.execute("file.new", json!({"width":4,"height":4})).unwrap();
    let before = s.active().unwrap().doc.clone();
    let n = s.active().unwrap().history.past_len();
    s.execute("plugin.session.io.test.photocraft-session/rename", json!({})).unwrap();
    let d = s.active().unwrap();
    assert_eq!(d.doc.layer(d.active_layer.unwrap()).unwrap().name, "Plugin renamed");
    assert_eq!(d.history.past_len(), n + 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.active().unwrap().doc, before);
    let permission = craft_ext_core::permission::Permission::parse("document.edit:document").unwrap();
    m.store.revoke(&m.app.clone().unwrap(), &id, &permission).unwrap();
    let err = s.execute("plugin.session.io.test.photocraft-session/rename", json!({})).unwrap_err();
    assert!(err.to_string().contains("disabled") || err.to_string().contains("Unknown"), "{err}");
    assert_eq!(s.active().unwrap().doc, before);
    m.grant(&id, permission).unwrap();
}
