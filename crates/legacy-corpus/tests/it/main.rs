mod coverage;
mod parity;

use std::path::Path;

use craft_testkit::corpus::Committed;
use photocraft_doc::{Document, LayerContent, Surface};
use photocraft_engine::Session;
use photocraft_legacy_corpus::{CANVAS, LAYER_PIXELS};
use photocraft_plugins::{Limits, Plugin, registry};

pub fn corpus() -> Committed {
    craft_testkit::corpus::open(&photocraft_legacy_corpus::root()).unwrap()
}

pub fn install(member: &str, file: &str) {
    registry::install(Plugin::load(&corpus().bytes(member, file).unwrap(), Limits::default()).unwrap());
}

pub fn document(member: &str) -> Document {
    photocraft_format::load_from_bytes(&corpus().bytes(member, "document.pcraft").unwrap()).unwrap()
}

pub fn session(member: &str) -> Session {
    let mut s = Session::new();
    s.add_document(document(member), None);
    s
}

pub fn samples8(surface: &Surface) -> Vec<Vec<u8>> {
    let n = surface.format().channels();
    surface.read_region(CANVAS).chunks(n).map(|px| px.iter().map(|v| (v * 255.0).round() as u8).collect()).collect()
}

pub fn only_layer(doc: &Document) -> &photocraft_doc::Layer {
    let layers = doc.walk();
    assert_eq!(layers.len(), 1, "the corpus documents have one layer");
    layers[0].2
}

pub fn layer_samples(s: &Session) -> Vec<Vec<u8>> {
    let layer = only_layer(&s.active().unwrap().doc);
    samples8(layer.surface().unwrap())
}

pub fn smart_cache(doc: &Document) -> Vec<Vec<u8>> {
    let LayerContent::Smart(object) = &only_layer(doc).content else { panic!("not a smart object") };
    samples8(object.cache.as_ref().unwrap())
}

pub fn original() -> Vec<Vec<u8>> {
    LAYER_PIXELS.iter().map(|px| px.to_vec()).collect()
}

pub fn copy_member(member: &str, to: &Path) {
    let corpus = corpus();
    let entry = corpus.manifest().entry(member).unwrap().clone();
    for file in &entry.files {
        let target = to.join(&file.path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, corpus.bytes(member, &file.path).unwrap()).unwrap();
    }
}
