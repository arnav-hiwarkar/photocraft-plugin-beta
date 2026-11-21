use std::sync::Arc;

use photocraft_doc::{
    Affine, AlphaChannel, BlendMode, ColorMode, DocId, Document, Layer, LayerContent, LayerId, LayerMask, PixelFormat, Rect, SampleType, Size, SmartContentsId,
    SmartFilter, SmartObject, SmartSource, Surface,
};
use photocraft_engine::{plugin_cmds, smart_cmds};
use photocraft_format::{PcraftWriter, SaveOptions};
use photocraft_plugins::{Limits, Plugin, registry};

use crate::Error;
use crate::modules::{ABSENT_ID, FILL_ID};

pub const GENERATOR: &str = "photocraft-format 0.23.1";
pub const CANVAS: Rect = Rect { x0: 0, y0: 0, x1: 4, y1: 2 };
pub const PROBE_CANVAS: Rect = Rect { x0: 0, y0: 0, x1: 64, y1: 4 };

pub const LAYER_PIXELS: [[u8; 4]; 8] =
    [[200, 40, 10, 255], [10, 200, 40, 128], [40, 10, 200, 64], [250, 250, 250, 16], [0, 0, 0, 255], [128, 128, 128, 255], [90, 60, 30, 200], [30, 60, 90, 32]];

pub const LAYER_ID: u64 = 2;
pub const UPPER_LAYER_ID: u64 = 3;
const DOC_ID: u64 = 1;
const CONTENTS_ID: u64 = 4;

fn doc_error(e: impl std::fmt::Display) -> Error {
    Error::Document(e.to_string())
}

fn layer_surface(format: PixelFormat) -> Surface {
    let mut surface = Surface::new(format);
    for (i, px) in LAYER_PIXELS.iter().enumerate() {
        let (x, y) = ((i % 4) as i32, (i / 4) as i32);
        surface.write_pixel(x, y, &px.map(|v| f32::from(v) / 255.0));
    }
    surface
}

fn gray(fill: &[(Rect, f32)]) -> Surface {
    let mut surface = Surface::new(PixelFormat::GRAY8);
    for (rect, v) in fill {
        surface.fill_rect(*rect, &[*v]);
    }
    surface
}

fn selection_pattern() -> Surface {
    gray(&[(Rect { x0: 0, y0: 0, x1: 1, y1: 2 }, 1.0), (Rect { x0: 1, y0: 0, x1: 2, y1: 2 }, 128.0 / 255.0)])
}

fn layer(id: u64, name: &str, content: LayerContent) -> Layer {
    let mut layer = Layer::new(name, content);
    layer.id = LayerId(id);
    layer
}

fn empty(name: &str) -> Document {
    let mut doc = Document::new(name, Size::new(4, 2), ColorMode::Rgb, SampleType::U8);
    doc.id = DocId(DOC_ID);
    doc
}

fn base(name: &str, layer_name: &str) -> Document {
    let mut doc = empty(name);
    let surface = layer_surface(doc.pixel_format());
    doc.layers.push(layer(LAYER_ID, layer_name, LayerContent::Raster(surface)));
    doc
}

pub fn save(doc: &Document) -> Result<Vec<u8>, Error> {
    PcraftWriter::new().with_generator(GENERATOR).save_zip(doc, &SaveOptions::default()).map(|(bytes, _)| bytes).map_err(doc_error)
}

fn with_layer(doc: &mut Document, f: impl FnOnce(&mut Layer)) {
    if let Some(layer) = doc.layer_mut(LayerId(LAYER_ID)) {
        f(layer);
    }
}

pub fn channel() -> Result<Vec<u8>, Error> {
    let mut doc = base("Corpus Channel", "Corpus Layer");
    doc.channels.push(AlphaChannel::new("Corpus Alpha", gray(&[(CANVAS, 1.0)])));
    save(&doc)
}

pub fn locked() -> Result<Vec<u8>, Error> {
    let mut doc = base("Corpus Locked", "Corpus Locked Layer");
    with_layer(&mut doc, |l| l.locks.transparency = true);
    save(&doc)
}

pub fn pixels_locked() -> Result<Vec<u8>, Error> {
    let mut doc = base("Corpus Pixels Locked", "Corpus Pixels Locked Layer");
    with_layer(&mut doc, |l| l.locks.pixels = true);
    save(&doc)
}

pub fn selection() -> Result<Vec<u8>, Error> {
    let mut doc = base("Corpus Selection", "Corpus Layer");
    doc.selection = Some(selection_pattern());
    save(&doc)
}

pub fn mask() -> Result<Vec<u8>, Error> {
    let mut doc = base("Corpus Mask", "Corpus Masked Layer");
    with_layer(&mut doc, |l| l.mask = Some(LayerMask { surface: gray(&[(CANVAS, 1.0)]), ..LayerMask::reveal_all() }));
    save(&doc)
}

pub fn quick_mask() -> Result<Vec<u8>, Error> {
    let mut doc = base("Corpus Quick Mask", "Corpus Layer");
    doc.quick_mask = Some(AlphaChannel::new("Quick Mask", gray(&[(CANVAS, 1.0)])));
    save(&doc)
}

pub fn two_layers() -> Result<Vec<u8>, Error> {
    let mut doc = base("Corpus Two Layers", "Corpus Lower Layer");
    let surface = layer_surface(doc.pixel_format());
    doc.layers.push(layer(UPPER_LAYER_ID, "Corpus Upper Layer", LayerContent::Raster(surface)));
    save(&doc)
}

pub fn probe() -> Result<Vec<u8>, Error> {
    let mut doc = Document::new("Corpus Probe", Size::new(64, 4), ColorMode::Rgb, SampleType::F32);
    doc.id = DocId(DOC_ID);
    let mut surface = Surface::new(doc.pixel_format());
    surface.fill_rect(PROBE_CANVAS, &[0.5, 0.5, 0.5, 1.0]);
    doc.layers.push(layer(LAYER_ID, "Corpus Probe Layer", LayerContent::Raster(surface)));
    save(&doc)
}

fn record(plugin: &str) -> SmartFilter {
    SmartFilter {
        command: plugin_cmds::RUN.into(),
        params: serde_json::json!({"id": plugin, "params": {}}),
        blend: BlendMode::Normal,
        opacity: 1.0,
        visible: true,
    }
}

fn smart(name: &str, filters: Vec<SmartFilter>, filter_mask: Option<LayerMask>) -> Result<Document, Error> {
    let contents = save(&base("Corpus Content", "Corpus Layer"))?;
    let mut object = SmartObject::new(SmartSource::Embedded { file_name: "Corpus Content.pcraft".into(), bytes: Arc::new(contents) }, Affine::IDENTITY, None);
    object.contents_id = SmartContentsId(CONTENTS_ID);
    object.smart_filters = filters;
    object.filter_mask = filter_mask;
    let mut doc = empty(name);
    doc.layers.push(layer(LAYER_ID, "Corpus Smart Object", LayerContent::Smart(object)));
    smart_cmds::refresh(&mut doc, LayerId(LAYER_ID)).map_err(doc_error)?;
    Ok(doc)
}

fn install_fill(fill: &[u8]) -> Result<(), Error> {
    registry::install(Plugin::load(fill, Limits::default()).map_err(doc_error)?);
    Ok(())
}

pub fn smart_filter(fill: &[u8]) -> Result<Vec<u8>, Error> {
    install_fill(fill)?;
    save(&smart("Corpus Smart Filter", vec![record(FILL_ID)], None)?)
}

pub fn smart_apply() -> Result<Vec<u8>, Error> {
    let mut doc = smart("Corpus Smart Apply", Vec::new(), None)?;
    doc.selection = Some(selection_pattern());
    save(&doc)
}

pub fn filter_mask(fill: &[u8]) -> Result<Vec<u8>, Error> {
    install_fill(fill)?;
    let mask = LayerMask { surface: gray(&[(CANVAS, 1.0)]), ..LayerMask::reveal_all() };
    save(&smart("Corpus Filter Mask", vec![record(FILL_ID)], Some(mask))?)
}

pub fn missing_plugin() -> Result<Vec<u8>, Error> {
    if registry::get(ABSENT_ID).is_some() {
        return Err(Error::Document(format!("{ABSENT_ID} must not be installed while the missing-plugin document is made")));
    }
    save(&smart("Corpus Missing Plug-in", vec![record(ABSENT_ID)], None)?)
}
