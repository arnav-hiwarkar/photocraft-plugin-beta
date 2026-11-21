use std::sync::Arc;

use photocraft_doc::{
    Affine, AlphaChannel, BlendMode, ColorMode, DocId, Document, Layer, LayerContent, LayerId, PixelFormat, Rect, SampleType, Size, SmartContentsId,
    SmartFilter, SmartObject, SmartSource, Surface,
};
use photocraft_engine::{plugin_cmds, smart_cmds};
use photocraft_plugins::{Limits, Plugin, registry};

use crate::Error;
use crate::modules::{ABSENT_ID, FILL_ID};

pub const CANVAS: Rect = Rect { x0: 0, y0: 0, x1: 4, y1: 2 };

pub const LAYER_PIXELS: [[u8; 4]; 8] =
    [[200, 40, 10, 255], [10, 200, 40, 128], [40, 10, 200, 64], [250, 250, 250, 16], [0, 0, 0, 255], [128, 128, 128, 255], [90, 60, 30, 200], [30, 60, 90, 32]];

const DOC_ID: u64 = 1;
const LAYER_ID: u64 = 2;
const CONTENTS_ID: u64 = 3;

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

fn base(name: &str, layer_name: &str) -> Document {
    let mut doc = Document::new(name, Size::new(4, 2), ColorMode::Rgb, SampleType::U8);
    doc.id = DocId(DOC_ID);
    let mut layer = Layer::new(layer_name, LayerContent::Raster(layer_surface(doc.pixel_format())));
    layer.id = LayerId(LAYER_ID);
    doc.layers.push(layer);
    doc
}

fn save(doc: &Document) -> Result<Vec<u8>, Error> {
    photocraft_format::save_to_bytes(doc, &photocraft_format::SaveOptions::default()).map_err(doc_error)
}

pub fn channel() -> Result<Vec<u8>, Error> {
    let mut doc = base("Corpus Channel", "Corpus Layer");
    let mut alpha = Surface::new(PixelFormat::GRAY8);
    alpha.fill_rect(CANVAS, &[1.0]);
    doc.channels.push(AlphaChannel::new("Corpus Alpha", alpha));
    save(&doc)
}

pub fn locked() -> Result<Vec<u8>, Error> {
    let mut doc = base("Corpus Locked", "Corpus Locked Layer");
    if let Some(layer) = doc.layer_mut(LayerId(LAYER_ID)) {
        layer.locks.transparency = true;
    }
    save(&doc)
}

pub fn selection() -> Result<Vec<u8>, Error> {
    let mut doc = base("Corpus Selection", "Corpus Layer");
    let mut selection = Surface::new(PixelFormat::GRAY8);
    selection.fill_rect(Rect { x0: 0, y0: 0, x1: 1, y1: 2 }, &[1.0]);
    selection.fill_rect(Rect { x0: 1, y0: 0, x1: 2, y1: 2 }, &[128.0 / 255.0]);
    doc.selection = Some(selection);
    save(&doc)
}

fn smart(name: &str, plugin: &str) -> Result<Document, Error> {
    let contents = base("Corpus Content", "Corpus Layer");
    let bytes = smart_cmds::encode_source(&contents).map_err(doc_error)?;
    let mut object = SmartObject::new(SmartSource::Embedded { file_name: "Corpus Content.pcraft".into(), bytes: Arc::new(bytes) }, Affine::IDENTITY, None);
    object.contents_id = SmartContentsId(CONTENTS_ID);
    object.smart_filters.push(SmartFilter {
        command: plugin_cmds::RUN.into(),
        params: serde_json::json!({"id": plugin, "params": {}}),
        blend: BlendMode::Normal,
        opacity: 1.0,
        visible: true,
    });
    let mut doc = Document::new(name, Size::new(4, 2), ColorMode::Rgb, SampleType::U8);
    doc.id = DocId(DOC_ID);
    let mut layer = Layer::new("Corpus Smart Object", LayerContent::Smart(object));
    layer.id = LayerId(LAYER_ID);
    doc.layers.push(layer);
    smart_cmds::refresh(&mut doc, LayerId(LAYER_ID)).map_err(doc_error)?;
    Ok(doc)
}

pub fn smart_filter(fill: &[u8]) -> Result<Vec<u8>, Error> {
    registry::install(Plugin::load(fill, Limits::default()).map_err(doc_error)?);
    save(&smart("Corpus Smart Filter", FILL_ID)?)
}

pub fn missing_plugin() -> Result<Vec<u8>, Error> {
    if registry::get(ABSENT_ID).is_some() {
        return Err(Error::Document(format!("{ABSENT_ID} must not be installed while the missing-plugin document is made")));
    }
    save(&smart("Corpus Missing Plug-in", ABSENT_ID)?)
}
