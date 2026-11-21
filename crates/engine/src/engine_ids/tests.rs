use photocraft_doc::{Document, LayerContent, SmartFilter, SmartObject};
use photocraft_raster::Surface;
use serde_json::{Value, json};

use super::*;
use crate::Session;
use crate::actions_cmds::{Action, ActionList, ActionStep, ActionsFile};

const OLD: &str = "filter.gaussianBlur";
const CURRENT: &str = "filter.blur.gaussianBlur";

fn radius_from_px(mut params: Value) -> Result<Value, String> {
    if let Some(o) = params.as_object_mut()
        && !o.contains_key("radius")
        && let Some(px) = o.remove("px")
    {
        o.insert("radius".into(), px);
    }
    Ok(params)
}

const RENAMED: &[Migration<Value>] = &[Migration { name: "radius-from-px", run: radius_from_px }];

fn renamed_lock() -> String {
    let radius = ArgsHash::of_keys(["radius"]);
    let shipped = format!("[[entry]]\nid = \"{CURRENT}\"\nclass = \"command\"\nargs = \"{radius}\"\n");
    assert!(LOCK.contains(&shipped), "the committed lock lists {CURRENT} with a radius argument");
    let history = format!(
        "[[entry]]\nid = \"{OLD}\"\ncurrent = \"{CURRENT}\"\nclass = \"command\"\nargs = \"{radius}\"\nmigrations = [{{ from = \"{}\", name = \"radius-from-px\" }}]\n",
        ArgsHash::of_keys(["px"])
    );
    LOCK.replace(&shipped, &history).replacen("format = 1\n", "format = 1\ntombstones = [\"filter.retired\"]\n", 1)
}

fn renamed<R>(f: impl FnOnce() -> R) -> R {
    simulate(EngineIds::new(&renamed_lock(), RENAMED).unwrap(), f)
}

fn grid() -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 24, "height": 16})).unwrap();
    s.execute("layer.new.layer", json!({"name": "grid"})).unwrap();
    s.edit("grid", |doc, active| {
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        let fmt = surf.format();
        for y in 0..16 {
            for x in 0..24 {
                let v = if x % 6 < 2 { 0.9 } else { 0.2 };
                surf.write_pixel(x, y, &photocraft_raster::from_rgba(&fmt, [v, v, v, 1.0]));
            }
        }
        Ok(())
    })
    .unwrap();
    s
}

fn layer_pixels(s: &Session) -> Surface {
    let d = s.active().unwrap();
    d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().clone()
}

fn smart(doc: &Document) -> &SmartObject {
    doc.walk().into_iter().find_map(|(_, _, l)| if let LayerContent::Smart(sm) = &l.content { Some(sm) } else { None }).unwrap()
}

fn filter(command: &str, params: Value) -> SmartFilter {
    SmartFilter { command: command.into(), params, blend: photocraft_color::BlendMode::Normal, opacity: 1.0, visible: true }
}

fn saved_with_filters(filters: Vec<SmartFilter>) -> Document {
    let mut s = grid();
    s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
    s.edit("fixture", |doc, active| {
        let LayerContent::Smart(sm) = &mut doc.layer_mut(active.unwrap()).unwrap().content else { panic!("a smart object") };
        sm.smart_filters = filters;
        sm.cache = None;
        Ok(())
    })
    .unwrap();
    let bytes = photocraft_format::save_to_bytes(&s.active().unwrap().doc, &Default::default()).unwrap();
    photocraft_format::load_from_bytes(&bytes).unwrap()
}

fn rendered(doc: &Document) -> Surface {
    crate::smart_cmds::render(doc, smart(doc)).unwrap().unwrap()
}

fn smart_filter_resolves() {
    let reopened = saved_with_filters(vec![filter(OLD, json!({"px": 3}))]);
    let current = saved_with_filters(vec![filter(CURRENT, json!({"radius": 3}))]);
    let unfiltered = saved_with_filters(vec![]);
    assert!(rendered(&reopened) == rendered(&current), "the old id and argument render as the current command");
    assert!(rendered(&reopened) != rendered(&unfiltered), "the filter ran");
    let resaved = photocraft_format::load_from_bytes(&photocraft_format::save_to_bytes(&reopened, &Default::default()).unwrap()).unwrap();
    assert_eq!((smart(&resaved).smart_filters[0].command.as_str(), &smart(&resaved).smart_filters[0].params), (OLD, &json!({"px": 3})));
}

fn action_step_resolves() {
    let fixture = ActionsFile::new(vec![Action::new("Soften", vec![ActionStep::new(OLD, json!({"px": 3}))])], Vec::new()).to_json().unwrap();
    let mut s = grid();
    s.actions.list = ActionList::load(ActionsFile::from_json(&fixture).unwrap()).0;
    let before = layer_pixels(&s);
    assert_eq!(s.execute("actions.play", json!({"action": "Soften"})).unwrap(), json!({"action": "Soften", "ran": 1}));
    let mut direct = grid();
    direct.execute(CURRENT, json!({"radius": 3})).unwrap();
    assert!(layer_pixels(&s) == layer_pixels(&direct), "the step replays as the current command");
    assert!(layer_pixels(&s) != before, "the step ran");
    assert_eq!(s.actions.list.actions()[0].steps[0].command, OLD, "the saved step keeps its id");
}

fn keymap_resolves() {
    let mut s = Session::new();
    s.load_prefs_json(&json!({"shortcuts": {OLD: "F9", "filter.never": "F10"}}).to_string()).unwrap();
    assert_eq!(s.prefs().shortcuts.get(CURRENT).map(String::as_str), Some("F9"));
    assert_eq!(s.prefs().shortcuts.get("filter.never").map(String::as_str), Some("F10"), "an unknown binding is kept as saved");
}

#[test]
fn every_load_path_resolves_a_renamed_command_and_argument() {
    renamed(|| {
        for site in Site::ALL {
            match site {
                Site::SmartFilter => smart_filter_resolves(),
                Site::ActionStep => action_step_resolves(),
                Site::Keymap => keymap_resolves(),
            }
        }
    });
}

#[test]
fn the_simulated_rename_is_a_lock_the_gate_accepts() {
    let registry: Vec<_> = registered().unwrap().into_iter().filter(|r| r.id.as_str() != "test.panic").collect();
    let lock = Lock::parse(&renamed_lock()).unwrap();
    assert_eq!(craft_idmap::check(&lock, &registry, &["radius-from-px"]), vec![]);
    assert_eq!(craft_idmap::check(&lock, &registry, &[]).len(), 1, "the migration must be registered");
}

#[test]
fn unknown_and_retired_ids_stay_inert_with_a_diagnostic() {
    renamed(|| {
        let unknown = saved_with_filters(vec![filter("filter.never", json!({"radius": 3}))]);
        assert!(rendered(&unknown) == rendered(&saved_with_filters(vec![])), "an unknown smart filter leaves the pixels alone");
        let row = crate::inspect::layer_detail(unknown.walk().into_iter().map(|(_, _, l)| l).find(|l| matches!(l.content, LayerContent::Smart(_))).unwrap());
        assert_eq!(row["smartFilters"][0]["unresolved"], json!("smart filter: unknown command `filter.never`"));
        assert_eq!(row["smartFilters"][0]["command"], json!("filter.never"));

        let mut s = grid();
        s.actions.list.push(Action::new("Gone", vec![ActionStep::new("filter.retired", json!({}))])).unwrap();
        let before = layer_pixels(&s);
        let played = s.execute("actions.play", json!({"action": "Gone"})).unwrap();
        assert_eq!(played["failed"]["error"], json!("action step: command `filter.retired` was retired"));
        assert_eq!(played["ran"], json!(0));
        assert!(layer_pixels(&s) == before);
    });
}

#[test]
fn the_shipped_lock_resolves_every_registered_id_to_itself() {
    for r in registered().unwrap().iter().filter(|r| r.id.as_str() != "test.panic") {
        assert_eq!(current_id(Site::Keymap, r.id.as_str()), Ok(r.id.as_str()));
    }
    assert_eq!(current_id(Site::Keymap, OLD), Err(Unresolved::Unknown { site: Site::Keymap, id: OLD.into() }));
}
