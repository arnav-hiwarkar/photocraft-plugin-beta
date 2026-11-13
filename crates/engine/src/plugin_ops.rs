//! The document operations of PhotoCraft's plugin contract (`photocraft_plugins::contract`), run on
//! the document thread inside a plugin event's transaction. Every argument is hostile: sizes are
//! capped before anything is allocated, layers are looked up rather than indexed, and every
//! failure is a recoverable `OpError` for the plugin.
//!
//! Edits go through the engine's own commands or `Session::edit`, so locks, history folding and
//! rollback work as they do for any other command; `plugin_sessions` folds an event's edits into
//! one history step (or a whole tool gesture into one).
use crate::{LayerId, Session};
use base64::Engine as _;
use craft_ext_host::api::{Code, OpError};
use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Layer, LayerContent};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Pixel bytes one `pixels.get` or `selection.mask.get` may return (the result is base64, under
/// the host's 8 MiB pending-result budget).
pub const MAX_READ_BYTES: usize = 4 * 1024 * 1024;
/// Layers one event may create.
pub const MAX_CREATED_LAYERS: u32 = 100;
/// Exports one event may stage.
pub const MAX_EXPORTS: u32 = 500;
/// Bytes of files one event may stage for writing.
pub const MAX_STAGED_BYTES: usize = 512 * 1024 * 1024;
/// Bytes `files.read` returns.
pub const MAX_FILE_READ: u64 = 4 * 1024 * 1024;
/// Guides a document may hold after `guide.add`.
pub const MAX_GUIDES: usize = 1000;
/// Histogram work is bounded independently of its small result. Larger images need a `rect`.
pub const MAX_HISTOGRAM_PIXELS: u64 = 64 * 1024 * 1024;

/// Adjustments `adjustment.apply` may run; the second flag says whether an adjustment layer
/// exists for it.
pub const ADJUSTMENTS: &[(&str, bool)] = &[
    ("brightnessContrast", true),
    ("levels", true),
    ("curves", true),
    ("exposure", true),
    ("vibrance", true),
    ("hueSaturation", true),
    ("colorBalance", true),
    ("blackWhite", true),
    ("photoFilter", true),
    ("channelMixer", true),
    ("invert", true),
    ("posterize", true),
    ("threshold", true),
    ("gradientMap", true),
    ("selectiveColor", true),
    ("desaturate", false),
    ("equalize", false),
];
/// Parameter keys an adjustment may not take from a plugin: targets, other layers, sampling
/// eyedroppers, paths and anything host-private.
const ADJUSTMENT_RESERVED: &[&str] = &["target", "layer", "layers", "eyedropper", "path", "document", "file", "coalesce"];
/// File extensions `files.write` refuses: things a desktop would run or load as code.
const UNSAFE_EXTENSIONS: &[&str] = &[
    "exe", "com", "bat", "cmd", "ps1", "psm1", "vbs", "js", "jse", "wsf", "msi", "msp", "scr", "sh", "bash", "zsh", "fish", "command", "app", "desktop", "so",
    "dll", "dylib", "lnk", "url", "jar", "py", "pl", "rb", "service", "plist",
];
/// Image formats `document.export` writes, by extension.
pub const EXPORT_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "tif", "tiff", "webp", "psd"];

/// A picked file or folder, owned by one plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picked {
    pub plugin: String,
    pub path: PathBuf,
    pub mode: PickMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickMode {
    Folder,
    Save,
    Open,
}

impl PickMode {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "folder" => Some(Self::Folder),
            "save" => Some(Self::Save),
            "open" => Some(Self::Open),
            _ => None,
        }
    }
}

/// A `files.pick` the person has to answer.
#[derive(Clone, Debug, PartialEq)]
pub struct PickRequest {
    pub request: u64,
    /// The session entry (`plugin.session.<plugin>/<contribution>`) that receives `files.picked`.
    pub owner: String,
    pub plugin: String,
    pub mode: PickMode,
    pub title: String,
    pub name: String,
}

/// Picked-file state of a session: handles, questions for the person and headless answers.
#[derive(Debug, Default)]
pub struct Picks {
    next_request: u64,
    next_handle: u64,
    handles: std::collections::BTreeMap<String, Picked>,
    /// Questions waiting for the desktop's dialog ([`Session::take_plugin_pick`]).
    pub requests: std::collections::VecDeque<PickRequest>,
    /// Paths that answer the next picks in order, for headless runs (`photocraft-cli run --pick`).
    pub answers: std::collections::VecDeque<PathBuf>,
    /// Whether a desktop shows file dialogs for this session.
    pub interactive: bool,
}

/// What an event stages until it commits: files to write and picks to ask.
#[derive(Debug, Default)]
pub struct Staged {
    pub writes: Vec<(PathBuf, Vec<u8>)>,
    pub bytes: usize,
    pub picks: Vec<PickRequest>,
    pub created_layers: u32,
    pub exports: u32,
}

/// Who calls an operation and what the event captured.
pub struct OpCtx<'a> {
    pub plugin: &'a str,
    pub owner: &'a str,
    pub selection: &'a Value,
    pub staged: &'a mut Staged,
}

fn invalid(m: impl Into<String>) -> OpError {
    OpError::invalid(m)
}
fn engine(e: impl std::fmt::Display) -> OpError {
    OpError::invalid(e.to_string())
}
fn budget(m: impl Into<String>) -> OpError {
    OpError::new(Code::Budget, m)
}

fn kind_name(content: &LayerContent) -> &'static str {
    match content {
        LayerContent::Raster(_) => "pixel",
        LayerContent::Group(_) => "group",
        LayerContent::Adjustment(_) => "adjustment",
        LayerContent::Fill(_) => "fill",
        LayerContent::Text(_) => "type",
        LayerContent::Shape(_) => "shape",
        LayerContent::Smart(_) => "smart-object",
    }
}

fn blend_name(l: &Layer) -> String {
    l.blend.label().to_ascii_lowercase().replace(' ', "-")
}

fn layer_json(l: &Layer, parent: Option<LayerId>) -> Value {
    let bounds = l.surface().map(|s| s.content_bounds()).filter(|r| !r.is_empty()).map(|r| json!([r.x0, r.y0, r.width(), r.height()]));
    json!({
        "id": l.id.0,
        "name": l.name,
        "kind": kind_name(&l.content),
        "parent": parent.map(|p| p.0),
        "visible": l.visible,
        "opacity": (l.opacity * 100.0).round(),
        "fill_opacity": (l.fill_opacity * 100.0).round(),
        "blend": blend_name(l),
        "bounds": bounds,
        "has_mask": l.mask.is_some(),
        "clipped": l.clipped,
        "locked": l.locks.all || l.locks.pixels,
    })
}

/// Every layer, top to bottom, with groups expanded (at most 64 levels deep).
fn list_layers(layers: &[Layer], parent: Option<LayerId>, depth: usize, out: &mut Vec<Value>) {
    if depth > 64 {
        return;
    }
    for l in layers.iter().rev() {
        out.push(layer_json(l, parent));
        if let LayerContent::Group(g) = &l.content {
            list_layers(&g.children, Some(l.id), depth + 1, out);
        }
    }
}

fn parent_of(layers: &[Layer], id: LayerId, parent: Option<LayerId>, depth: usize) -> Option<Option<LayerId>> {
    if depth > 64 {
        return None;
    }
    for l in layers {
        if l.id == id {
            return Some(parent);
        }
        if let LayerContent::Group(g) = &l.content {
            if let Some(p) = parent_of(&g.children, id, Some(l.id), depth + 1) {
                return Some(p);
            }
        }
    }
    None
}

fn uint(args: &Value, key: &str) -> Result<Option<u64>, OpError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| invalid(format!("`{key}` must be a whole number"))),
    }
}

fn int(v: &Value, what: &str) -> Result<i64, OpError> {
    let f = v.as_f64().filter(|f| f.is_finite() && f.fract() == 0.0 && f.abs() <= 1e9).ok_or_else(|| invalid(format!("`{what}` must be a whole number")))?;
    Ok(f as i64)
}

/// `[x, y, width, height]` (whole numbers, width and height at least 1) as a rectangle.
fn rect_of(v: &Value, what: &str) -> Result<Rect, OpError> {
    let a = v.as_array().filter(|a| a.len() == 4).ok_or_else(|| invalid(format!("`{what}` must be [x, y, width, height]")))?;
    let (x, y, w, h) = (int(&a[0], what)?, int(&a[1], what)?, int(&a[2], what)?, int(&a[3], what)?);
    if w < 1 || h < 1 || w > 1_000_000 || h > 1_000_000 {
        return Err(invalid(format!("`{what}` needs a width and height from 1 to 1000000")));
    }
    Ok(Rect::from_xywh(x as i32, y as i32, w as u32, h as u32))
}

/// The `rect` argument clipped to the image, or the whole image.
fn area(args: &Value, bounds: Rect) -> Result<Rect, OpError> {
    let r = match args.get("rect") {
        None | Some(Value::Null) => bounds,
        Some(v) => rect_of(v, "rect")?.intersect(&bounds),
    };
    if r.is_empty() {
        return Err(invalid("The area is outside the image"));
    }
    Ok(r)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Rgba8,
    Rgba16,
    Rgba32f,
}

impl Format {
    fn parse(args: &Value) -> Result<Self, OpError> {
        match args.get("format").and_then(Value::as_str).unwrap_or("rgba8") {
            "rgba8" => Ok(Self::Rgba8),
            "rgba16" => Ok(Self::Rgba16),
            "rgba32f" => Ok(Self::Rgba32f),
            other => Err(invalid(format!("Unknown pixel format `{other}` (rgba8, rgba16 or rgba32f)"))),
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Rgba8 => "rgba8",
            Self::Rgba16 => "rgba16",
            Self::Rgba32f => "rgba32f",
        }
    }
    fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgba8 => 4,
            Self::Rgba16 => 8,
            Self::Rgba32f => 16,
        }
    }
    /// Normalised RGBA samples to bytes. Integer formats clamp to 0..=1.
    fn encode(self, rgba: &[f32]) -> Vec<u8> {
        let mut out = Vec::with_capacity(rgba.len() * self.bytes_per_pixel() / 4);
        for &v in rgba {
            let v = if v.is_finite() { v } else { 0.0 };
            match self {
                Self::Rgba8 => out.push((v.clamp(0.0, 1.0) * 255.0).round() as u8),
                Self::Rgba16 => out.extend_from_slice(&((v.clamp(0.0, 1.0) * 65535.0).round() as u16).to_le_bytes()),
                Self::Rgba32f => out.extend_from_slice(&v.to_le_bytes()),
            }
        }
        out
    }
    fn decode(self, bytes: &[u8]) -> Result<Vec<f32>, OpError> {
        Ok(match self {
            Self::Rgba8 => bytes.iter().map(|&b| f32::from(b) / 255.0).collect(),
            Self::Rgba16 => bytes.chunks_exact(2).map(|c| f32::from(u16::from_le_bytes([c[0], c[1]])) / 65535.0).collect(),
            Self::Rgba32f => {
                let v: Vec<f32> = bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
                if v.iter().any(|x| !x.is_finite() || x.abs() > 65504.0) {
                    return Err(invalid("rgba32f values must be finite and at most 65504"));
                }
                v
            }
        })
    }
}

/// Interleaved samples with `n` channels (RGB or RGBA) to straight RGBA.
fn to_rgba(samples: &[f32], n: usize) -> Vec<f32> {
    if n == 4 {
        return samples.to_vec();
    }
    let mut out = Vec::with_capacity(samples.len() / n.max(1) * 4);
    for px in samples.chunks_exact(n.max(1)) {
        let g = |i: usize| px.get(i).copied().unwrap_or(0.0);
        out.extend_from_slice(&[g(0), g(1), g(2), 1.0]);
    }
    out
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn unb64(v: &Value, what: &str) -> Result<Vec<u8>, OpError> {
    let s = v.as_str().ok_or_else(|| invalid(format!("`{what}` must be a base64 string")))?;
    base64::engine::general_purpose::STANDARD.decode(s).map_err(|e| invalid(format!("`{what}` is not valid base64: {e}")))
}

/// A plain file name for a picked folder: no separators, not hidden, no executable extension.
pub fn safe_file_name(name: &str) -> Result<&str, OpError> {
    let bad = |why: &str| invalid(format!("Invalid file name {name:?}: {why}"));
    if name.is_empty() || name.len() > 255 {
        return Err(bad("use 1 to 255 bytes"));
    }
    if name.starts_with('.') || name.ends_with('.') || name.ends_with(' ') {
        return Err(bad("hidden names and names ending in a dot or space are refused"));
    }
    if name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')) {
        return Err(bad("it contains a separator or reserved character"));
    }
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if UNSAFE_EXTENSIONS.contains(&ext.as_str()) {
        return Err(bad("executable and script files are refused"));
    }
    Ok(name)
}

impl Picks {
    /// Registers a picked path for `plugin` and returns its opaque handle.
    pub fn grant(&mut self, plugin: &str, path: PathBuf, mode: PickMode) -> String {
        self.next_handle = self.next_handle.saturating_add(1);
        let handle = format!("h{}", self.next_handle);
        self.handles.insert(handle.clone(), Picked { plugin: plugin.to_string(), path, mode });
        handle
    }
    fn request_id(&mut self) -> u64 {
        self.next_request = self.next_request.saturating_add(1);
        self.next_request
    }
    /// The picked item behind `handle`, if it belongs to `plugin`.
    fn get(&self, plugin: &str, args: &Value) -> Result<Picked, OpError> {
        let handle = args.get("handle").and_then(Value::as_str).ok_or_else(|| invalid("`handle` is missing"))?;
        self.handles.get(handle).filter(|p| p.plugin == plugin).cloned().ok_or_else(|| OpError::new(Code::NotFound, "Unknown file handle: ask with files.pick"))
    }
}

/// Where a write goes: the picked save file, or `name` inside the picked folder.
fn target(p: &Picked, args: &Value) -> Result<PathBuf, OpError> {
    let path = match p.mode {
        PickMode::Save => {
            safe_file_name(p.path.file_name().and_then(|n| n.to_str()).ok_or_else(|| invalid("The save file needs a UTF-8 name"))?)?;
            p.path.clone()
        }
        PickMode::Folder => {
            p.path.join(safe_file_name(args.get("name").and_then(Value::as_str).ok_or_else(|| invalid("Writing into a folder needs `name`"))?)?)
        }
        PickMode::Open => return Err(OpError::denied("This handle is for reading")),
    };
    // Never follow a link someone planted at the target.
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(OpError::denied("The target is a link"));
    }
    Ok(path)
}

impl Staged {
    /// Publish in order, stopping at the first failure. Already published files remain; this is
    /// deliberately not a multi-file transaction. The event must reject commit and roll back its
    /// document/UI on an error, and tell the caller how much was published.
    pub(crate) fn publish_files(&mut self) -> Result<(), OpError> {
        for (published, (path, bytes)) in std::mem::take(&mut self.writes).into_iter().enumerate() {
            #[cfg(not(target_arch = "wasm32"))]
            let result = photocraft_format::atomic_write_no_follow(&path, &bytes);
            #[cfg(target_arch = "wasm32")]
            let result: std::io::Result<()> = {
                let _ = &bytes;
                Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "No file system on the web"))
            };
            if let Err(e) = result {
                return Err(OpError::new(
                    Code::Invalid,
                    format!(
                        "Could not publish {}: {e}; {published} file(s) already published and remain on disk; the failing destination may also have changed; document and UI changes rolled back",
                        path.display()
                    ),
                ));
            }
        }
        Ok(())
    }

    fn write(&mut self, path: PathBuf, bytes: Vec<u8>) -> Result<(), OpError> {
        let total = self.bytes.saturating_add(bytes.len());
        if total > MAX_STAGED_BYTES {
            return Err(budget("This event writes more than 512 MiB"));
        }
        self.bytes = total;
        self.writes.retain(|(p, _)| *p != path);
        self.writes.push((path, bytes));
        Ok(())
    }
}

impl Session {
    fn plugin_layer(&self, args: &Value) -> Result<LayerId, OpError> {
        let d = self.active().ok_or_else(|| OpError::new(Code::NotFound, "No document"))?;
        let id = match uint(args, "layer")? {
            Some(id) => LayerId(id),
            None => d.active_layer.ok_or_else(|| OpError::new(Code::NotFound, "No active layer"))?,
        };
        d.doc.layer(id).ok_or_else(|| OpError::new(Code::NotFound, format!("No layer {}", id.0)))?;
        Ok(id)
    }

    fn rgb_document(&self) -> Result<&crate::DocState, OpError> {
        let d = self.active().ok_or_else(|| OpError::new(Code::NotFound, "No document"))?;
        if d.doc.mode != ColorMode::Rgb {
            return Err(invalid("Pixel operations work on RGB documents"));
        }
        Ok(d)
    }

    /// Straight RGBA of the layer or composite inside `r`.
    fn plugin_read_rgba(&self, args: &Value, r: Rect) -> Result<Vec<f32>, OpError> {
        let d = self.rgb_document()?;
        if args.get("composite").and_then(Value::as_bool).unwrap_or(false) {
            let fmt = d.doc.pixel_format();
            let s = photocraft_compose::composite_to_surface(&d.doc, r, PixelFormat::new(fmt.mode, fmt.sample, true), None);
            return Ok(to_rgba(&s.read_region(r), s.channels()));
        }
        let id = self.plugin_layer(args)?;
        let l = d.doc.layer(id).ok_or_else(|| OpError::new(Code::NotFound, "No layer"))?;
        let LayerContent::Raster(s) = &l.content else {
            return Err(invalid(format!("Layer {} is not a pixel layer", id.0)));
        };
        Ok(to_rgba(&s.read_region(r), s.channels()))
    }

    /// Runs one contract operation. `ui.*` and `canvas.overlay.set` are handled by the caller.
    pub(crate) fn plugin_op(&mut self, op: &str, args: Value, cx: &mut OpCtx<'_>) -> Result<Value, OpError> {
        // Gesture cancellation can restore the document, but cannot undo external I/O.
        if self.plugin_gesture.is_some() && (op.starts_with("files.") || matches!(op, "document.export" | "layer.import")) {
            return Err(OpError::denied("File operations are unavailable during a tool gesture"));
        }
        match op {
            "plugin.resources" => return photocraft_plugins::resources::query().map_err(engine).and_then(|r| serde_json::to_value(r).map_err(engine)),
            "tool.state" => return Ok(self.plugin_gesture.as_ref().map_or(Value::Null, |g| g.payload.clone())),
            "documents.list" => {
                let active = self.active().map(|d| d.doc.id);
                return Ok(json!(
                    self.docs
                        .iter()
                        .map(|d| json!({"id": d.doc.id.0, "name": d.doc.name, "width": d.doc.size.width, "height": d.doc.size.height, "active": Some(d.doc.id) == active}))
                        .collect::<Vec<_>>()
                ));
            }
            "files.pick" => return self.plugin_pick(args, cx),
            "files.write" => {
                let p = self.plugin_picks.get(cx.plugin, &args)?;
                let path = target(&p, &args)?;
                let bytes = match (args.get("text"), args.get("data")) {
                    (Some(Value::String(t)), None) => t.as_bytes().to_vec(),
                    (None, Some(d)) => unb64(d, "data")?,
                    _ => return Err(invalid("Give exactly one of `text` or `data`")),
                };
                let n = bytes.len();
                cx.staged.write(path, bytes)?;
                return Ok(json!({"bytes": n}));
            }
            "files.read" => {
                let p = self.plugin_picks.get(cx.plugin, &args)?;
                if p.mode != PickMode::Open {
                    return Err(OpError::denied("This handle is not a file to read"));
                }
                use std::io::Read as _;
                let mut bytes = Vec::new();
                std::fs::File::open(&p.path).and_then(|f| f.take(MAX_FILE_READ + 1).read_to_end(&mut bytes)).map_err(engine)?;
                if bytes.len() as u64 > MAX_FILE_READ {
                    return Err(budget("The file is larger than 4 MiB"));
                }
                let name = p.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                return Ok(json!({"name": name, "bytes": bytes.len(), "data": b64(&bytes)}));
            }
            _ => {}
        }
        let d = self.active().ok_or_else(|| OpError::new(Code::NotFound, "No document"))?;
        match op {
            "document.info" => Ok(json!({
                "id": d.doc.id.0,
                "name": d.doc.name,
                "width": d.doc.size.width,
                "height": d.doc.size.height,
                "mode": format!("{:?}", d.doc.mode).to_ascii_lowercase(),
                "depth": match d.doc.depth { SampleType::U8 => 8, SampleType::U16 => 16, SampleType::F32 => 32 },
                "resolution": d.doc.resolution_dpi,
                "revision": d.revision,
                "active_layer": d.active_layer.map(|l| l.0),
                "layer_count": d.doc.layer_count(),
                "has_selection": d.doc.selection.is_some(),
            })),
            "layer.list" => {
                let mut out = vec![];
                list_layers(&d.doc.layers, None, 0, &mut out);
                Ok(json!(out))
            }
            "layer.get" => {
                let id = self.plugin_layer(&args)?;
                let l = d.doc.layer(id).ok_or_else(|| OpError::new(Code::NotFound, "No layer"))?;
                Ok(layer_json(l, parent_of(&d.doc.layers, id, None, 0).flatten()))
            }
            "selection.get" => Ok(cx.selection.clone()),
            "selection.mask.get" => {
                let r = area(&args, d.doc.bounds())?;
                if (r.width() as usize).saturating_mul(r.height() as usize) > MAX_READ_BYTES {
                    return Err(budget("At most 4 Mi pixels per call: pass a smaller `rect`"));
                }
                let bytes: Vec<u8> = match &d.doc.selection {
                    Some(s) => s.read_region(r).iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect(),
                    None => vec![0; r.width() as usize * r.height() as usize],
                };
                Ok(json!({"x": r.x0, "y": r.y0, "width": r.width(), "height": r.height(), "format": "gray8", "data": b64(&bytes)}))
            }
            "pixels.get" => {
                let format = Format::parse(&args)?;
                let r = area(&args, d.doc.bounds())?;
                if (r.width() as usize).saturating_mul(r.height() as usize).saturating_mul(format.bytes_per_pixel()) > MAX_READ_BYTES {
                    return Err(budget("At most 4 MiB of pixels per call: pass a smaller `rect` or read in tiles"));
                }
                let rgba = self.plugin_read_rgba(&args, r)?;
                Ok(json!({"x": r.x0, "y": r.y0, "width": r.width(), "height": r.height(), "format": format.name(), "data": b64(&format.encode(&rgba))}))
            }
            "histogram.get" => {
                let r = area(&args, d.doc.bounds())?;
                if u64::from(r.width()) * u64::from(r.height()) > MAX_HISTOGRAM_PIXELS {
                    return Err(budget("Histograms cover at most 64 Mi pixels: pass a smaller `rect`"));
                }
                let mut histogram = Histogram::new();
                // At most one 256×256 tile of float samples is materialized at a time.
                for y in (r.y0..r.y1).step_by(256) {
                    for x in (r.x0..r.x1).step_by(256) {
                        let tile = Rect::from_xywh(x, y, 256, 256).intersect(&r);
                        let mask = (args.get("selection").and_then(Value::as_bool) != Some(false))
                            .then(|| d.doc.selection.as_ref().map(|s| s.read_region(tile)))
                            .flatten();
                        let rgba = self.plugin_read_rgba(&args, tile)?;
                        histogram.add(&rgba, mask.as_deref());
                    }
                }
                Ok(histogram.finish())
            }
            "guides.list" => Ok(json!({"horizontal": d.doc.guides.horizontal, "vertical": d.doc.guides.vertical})),
            "channels.list" => {
                let colors: &[&str] = match d.doc.mode {
                    ColorMode::Rgb => &["Red", "Green", "Blue"],
                    ColorMode::Cmyk => &["Cyan", "Magenta", "Yellow", "Black"],
                    ColorMode::Lab => &["Lightness", "a", "b"],
                    _ => &["Gray"],
                };
                let mut out: Vec<Value> = colors.iter().enumerate().map(|(i, n)| json!({"index": i, "name": n, "kind": "color"})).collect();
                out.extend(
                    d.doc
                        .channels
                        .iter()
                        .enumerate()
                        .map(|(i, c)| json!({"index": colors.len() + i, "name": c.name, "kind": if c.spot.is_some() { "spot" } else { "alpha" }})),
                );
                Ok(json!(out))
            }
            "history.list" => Ok(json!({"entries": d.history.entries(), "current": d.history.past_len()})),
            "layer.rename" => {
                let target = self.plugin_layer(&args)?;
                let name = args["name"].as_str().filter(|s| !s.is_empty() && s.len() <= 4096).ok_or_else(|| invalid("Invalid name"))?;
                self.execute("layer.renameLayer", json!({"layer": target.0, "name": name})).map_err(engine)
            }
            "layer.create" => self.plugin_create_layer(&args, cx),
            "layer.delete" => {
                let id = uint(&args, "layer")?.ok_or_else(|| invalid("`layer` is required"))?;
                let id = self.plugin_layer(&json!({"layer": id}))?;
                self.execute("layer.delete", json!({"layer": id.0})).map(|_| Value::Null).map_err(engine)
            }
            "layer.set" => {
                let id = self.plugin_layer(&args)?;
                let mut props = serde_json::Map::new();
                props.insert("layer".into(), json!(id.0));
                if let Some(v) = args.get("name") {
                    let name = v.as_str().filter(|s| !s.is_empty() && s.len() <= 4096).ok_or_else(|| invalid("Invalid name"))?;
                    props.insert("name".into(), json!(name));
                }
                if let Some(v) = args.get("visible") {
                    props.insert("visible".into(), json!(v.as_bool().ok_or_else(|| invalid("`visible` must be true or false"))?));
                }
                for (key, to) in [("opacity", "opacity"), ("fill_opacity", "fill")] {
                    if let Some(v) = args.get(key) {
                        let pct = v.as_f64().filter(|p| (0.0..=100.0).contains(p)).ok_or_else(|| invalid(format!("`{key}` must be 0 to 100")))?;
                        props.insert(to.into(), json!(pct / 100.0));
                    }
                }
                if let Some(v) = args.get("blend") {
                    let name = v.as_str().ok_or_else(|| invalid("`blend` must be a name"))?;
                    crate::commands::blend_from_str(name).ok_or_else(|| invalid(format!("Unknown blend mode `{name}`")))?;
                    props.insert("blend".into(), json!(name));
                }
                if props.len() == 1 {
                    return Err(invalid("Nothing to change"));
                }
                self.execute("layer.setProps", Value::Object(props)).map(|_| Value::Null).map_err(engine)
            }
            "layer.select" => {
                let id = self.plugin_layer(&args)?;
                self.execute("layer.select", json!({"layer": id.0})).map(|_| Value::Null).map_err(engine)
            }
            "pixels.set" => self.plugin_set_pixels(&args),
            "selection.set" => self.plugin_set_selection(&args),
            "selection.save" => {
                if d.doc.selection.is_none() {
                    return Err(invalid("There is no pixel selection to save"));
                }
                let mut p = json!({});
                if let Some(name) = args.get("name") {
                    p["name"] = json!(name.as_str().filter(|s| !s.is_empty() && s.len() <= 255).ok_or_else(|| invalid("Invalid channel name"))?);
                }
                self.execute("select.saveSelection", p).map(|_| Value::Null).map_err(engine)
            }
            "guide.add" => {
                let vertical = match args.get("orientation").and_then(Value::as_str) {
                    Some("vertical") => true,
                    Some("horizontal") => false,
                    _ => return Err(invalid("`orientation` is horizontal or vertical")),
                };
                let extent = f64::from(if vertical { d.doc.size.width } else { d.doc.size.height });
                let at = args
                    .get("position")
                    .and_then(Value::as_f64)
                    .filter(|p| p.is_finite() && p.abs() <= extent * 10.0 + 10.0)
                    .ok_or_else(|| invalid("`position` must be a number near the image"))?;
                if d.doc.guides.horizontal.len() + d.doc.guides.vertical.len() >= MAX_GUIDES {
                    return Err(budget("The document already has 1000 guides"));
                }
                self.execute("view.newGuide", json!({"orientation": if vertical { "vertical" } else { "horizontal" }, "position": at}))
                    .map(|_| Value::Null)
                    .map_err(engine)
            }
            "guides.clear" => {
                if d.doc.guides.horizontal.is_empty() && d.doc.guides.vertical.is_empty() {
                    return Ok(Value::Null);
                }
                self.execute("view.clearGuides", json!({})).map(|_| Value::Null).map_err(engine)
            }
            "mask.add" => {
                let id = self.plugin_layer(&args)?;
                let cmd = match args.get("reveal").and_then(Value::as_str).unwrap_or("all") {
                    "all" => "layer.layerMask.revealAll",
                    "none" => "layer.layerMask.hideAll",
                    "selection" => "layer.layerMask.revealSelection",
                    other => return Err(invalid(format!("Unknown `reveal` {other:?} (all, none or selection)"))),
                };
                self.execute(cmd, json!({"layer": id.0})).map(|_| Value::Null).map_err(engine)
            }
            "filter.apply" => {
                // Only the allowlisted pixel filters run, with validated params (see session_filters).
                let (filter, params) = photocraft_plugins::session_filters::check(&args).map_err(|refusal| match refusal {
                    photocraft_plugins::session_filters::Refusal::NotAllowed(msg) => OpError::denied(msg),
                    photocraft_plugins::session_filters::Refusal::Invalid(msg) => OpError::invalid(msg),
                })?;
                self.execute(filter, params).map_err(engine)
            }
            "adjustment.apply" => {
                let (cmd, params) = adjustment_command(&args)?;
                self.execute(&cmd, params).map_err(engine)
            }
            "document.export" => {
                let p = self.plugin_picks.get(cx.plugin, &args)?;
                let path = target(&p, &args)?;
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                if !EXPORT_EXTENSIONS.contains(&ext.as_str()) {
                    return Err(invalid(format!("Export writes {} files", EXPORT_EXTENSIONS.join(", "))));
                }
                if cx.staged.exports >= MAX_EXPORTS {
                    return Err(budget("At most 500 exports per event"));
                }
                let doc = match uint(&args, "layer")? {
                    Some(id) => {
                        let id = self.plugin_layer(&json!({"layer": id}))?;
                        crate::layer_menu_cmds::layer_document(&d.doc, id).map_err(engine)?
                    }
                    None => (*d.doc).clone(),
                };
                let name = path.to_string_lossy().into_owned();
                let out = photocraft_io::export(&doc, &name, &Default::default()).map_err(engine)?;
                let n = out.bytes.len();
                cx.staged.exports += 1;
                cx.staged.write(path, out.bytes)?;
                Ok(json!({"bytes": n, "width": doc.size.width, "height": doc.size.height}))
            }
            "layer.import" => {
                let p = self.plugin_picks.get(cx.plugin, &args)?;
                if p.mode != PickMode::Open {
                    return Err(OpError::denied("This handle is not a file to read"));
                }
                self.execute("file.placeEmbedded", json!({"path": p.path.to_string_lossy()})).map_err(engine)
            }
            _ => Err(OpError::new(Code::NotFound, "Unknown operation")),
        }
    }

    fn plugin_create_layer(&mut self, args: &Value, cx: &mut OpCtx<'_>) -> Result<Value, OpError> {
        if cx.staged.created_layers >= MAX_CREATED_LAYERS {
            return Err(budget("At most 100 new layers per event"));
        }
        let name = match args.get("name") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.as_str().filter(|s| !s.is_empty() && s.len() <= 4096).ok_or_else(|| invalid("Invalid name"))?.to_string()),
        };
        let opacity = match args.get("opacity") {
            None => 100.0,
            Some(v) => v.as_f64().filter(|p| (0.0..=100.0).contains(p)).ok_or_else(|| invalid("`opacity` must be 0 to 100"))?,
        };
        let blend = match args.get("blend") {
            None => None,
            Some(v) => {
                let b = v.as_str().ok_or_else(|| invalid("`blend` must be a name"))?;
                crate::commands::blend_from_str(b).ok_or_else(|| invalid(format!("Unknown blend mode `{b}`")))?;
                Some(b.to_string())
            }
        };
        let before: std::collections::BTreeSet<u64> = self.active().map(|d| d.doc.walk().iter().map(|(_, _, l)| l.id.0).collect()).unwrap_or_default();
        let mut p = json!({});
        if let Some(n) = &name {
            p["name"] = json!(n);
        }
        let cmd = match args.get("kind").and_then(Value::as_str).unwrap_or("pixel") {
            "pixel" => {
                p["opacity"] = json!(opacity / 100.0);
                if let Some(b) = &blend {
                    p["mode"] = json!(b);
                }
                "layer.new.layer"
            }
            "group" => {
                p["opacity"] = json!(opacity);
                if let Some(b) = &blend {
                    p["mode"] = json!(b);
                }
                "layer.new.group"
            }
            "fill" => {
                let c = args
                    .get("color")
                    .and_then(Value::as_array)
                    .filter(|a| a.len() == 3 || a.len() == 4)
                    .ok_or_else(|| invalid("A fill layer needs `color` [r, g, b]"))?;
                let ch = |i: usize| c.get(i).and_then(Value::as_f64).filter(|v| (0.0..=1.0).contains(v)).ok_or_else(|| invalid("`color` channels are 0 to 1"));
                let hex = format!("#{:02x}{:02x}{:02x}", (ch(0)? * 255.0).round() as u8, (ch(1)? * 255.0).round() as u8, (ch(2)? * 255.0).round() as u8);
                self.execute("layer.newFillLayer.solidColor", json!({"color": hex})).map_err(engine)?;
                let id = self.new_layer_id(&before)?;
                let mut props = json!({"layer": id.0, "opacity": opacity / 100.0});
                if let Some(n) = &name {
                    props["name"] = json!(n);
                }
                if let Some(b) = &blend {
                    props["blend"] = json!(b);
                }
                self.execute("layer.setProps", props).map_err(engine)?;
                cx.staged.created_layers += 1;
                return Ok(json!({"id": id.0}));
            }
            other => return Err(invalid(format!("Unknown layer kind `{other}` (pixel, group or fill)"))),
        };
        self.execute(cmd, p).map_err(engine)?;
        cx.staged.created_layers += 1;
        Ok(json!({"id": self.new_layer_id(&before)?.0}))
    }

    /// The layer a create command just added: the active one, if it is new.
    fn new_layer_id(&self, before: &std::collections::BTreeSet<u64>) -> Result<LayerId, OpError> {
        let d = self.active().ok_or_else(|| OpError::new(Code::NotFound, "No document"))?;
        d.active_layer
            .filter(|l| !before.contains(&l.0))
            .or_else(|| d.doc.walk().iter().map(|(_, _, l)| l.id).find(|l| !before.contains(&l.0)))
            .ok_or_else(|| OpError::new(Code::Internal, "The new layer was not found"))
    }

    fn plugin_set_pixels(&mut self, args: &Value) -> Result<Value, OpError> {
        let format = Format::parse(args)?;
        let id = self.plugin_layer(args)?;
        let d = self.rgb_document()?;
        let x = int(args.get("x").unwrap_or(&Value::Null), "x")?;
        let y = int(args.get("y").unwrap_or(&Value::Null), "y")?;
        let w = int(args.get("width").unwrap_or(&Value::Null), "width")?;
        let h = int(args.get("height").unwrap_or(&Value::Null), "height")?;
        if w < 1 || h < 1 || w > 1_000_000 || h > 1_000_000 {
            return Err(invalid("`width` and `height` must be at least 1"));
        }
        let r = Rect::from_xywh(x as i32, y as i32, w as u32, h as u32);
        if r.intersect(&d.doc.bounds()) != r {
            return Err(invalid("The area must lie inside the image"));
        }
        let bytes = unb64(args.get("data").unwrap_or(&Value::Null), "data")?;
        let want = (w as usize).saturating_mul(h as usize).saturating_mul(format.bytes_per_pixel());
        if bytes.len() != want {
            return Err(invalid(format!("`data` has {} bytes; {}×{} {} needs {want}", bytes.len(), w, h, format.name())));
        }
        let rgba = format.decode(&bytes)?;
        let blend = args.get("selection").and_then(Value::as_bool).unwrap_or(false);
        let locks = d.doc.effective_locks(id);
        let name = d.doc.layer(id).map(|l| l.name.clone()).unwrap_or_default();
        if locks.pixels || locks.all {
            return Err(OpError::denied(format!("The layer \"{name}\" is locked")));
        }
        self.edit("Set Pixels", |doc, _| {
            let mask = if blend { doc.selection.as_ref().map(|s| s.read_region(r)) } else { None };
            let l = doc.layer_mut(id).ok_or(crate::EngineError::NoLayer(id))?;
            let LayerContent::Raster(surf) = &mut l.content else {
                return Err(crate::EngineError::Other(format!("Layer {} is not a pixel layer", id.0)));
            };
            let n = surf.channels();
            let integer = surf.format().sample != SampleType::F32;
            let mut out = surf.read_region(r);
            for (i, (px, new)) in out.chunks_exact_mut(n).zip(rgba.chunks_exact(4)).enumerate() {
                let t = if blend { mask.as_ref().map_or(0.0, |m| m.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0)) } else { 1.0 };
                for (c, v) in px.iter_mut().enumerate() {
                    if n == 4 && c == 3 && locks.transparency {
                        continue;
                    }
                    let src = new.get(c).copied().unwrap_or(1.0);
                    let src = if integer { src.clamp(0.0, 1.0) } else { src };
                    *v += (src - *v) * t;
                }
            }
            if out.len() != r.width() as usize * r.height() as usize * n {
                return Err(crate::EngineError::Other("Pixel buffer size mismatch".into()));
            }
            surf.write_region(r, &out);
            surf.prune();
            Ok(())
        })
        .map_err(engine)?;
        Ok(Value::Null)
    }

    fn plugin_set_selection(&mut self, args: &Value) -> Result<Value, OpError> {
        let mode = args.get("mode").and_then(Value::as_str).unwrap_or("replace");
        if !matches!(mode, "replace" | "add" | "subtract" | "intersect") {
            return Err(invalid(format!("Unknown `mode` {mode:?} (replace, add, subtract or intersect)")));
        }
        let shapes = ["all", "none", "rect", "ellipse", "mask"].iter().filter(|k| args.get(**k).is_some()).count();
        if shapes != 1 {
            return Err(invalid("Give exactly one of `all`, `none`, `rect`, `ellipse` or `mask`"));
        }
        if args.get("all").is_some_and(|v| v == &json!(true)) {
            return self.execute("select.all", json!({})).map(|_| Value::Null).map_err(engine);
        }
        if args.get("none").is_some_and(|v| v == &json!(true)) {
            return self.execute("select.deselect", json!({})).map(|_| Value::Null).map_err(engine);
        }
        for (key, ellipse) in [("rect", false), ("ellipse", true)] {
            if let Some(v) = args.get(key) {
                let r = rect_of(v, key)?;
                return self
                    .execute("select.rect", json!({"x": r.x0, "y": r.y0, "width": r.width(), "height": r.height(), "mode": mode, "ellipse": ellipse}))
                    .map(|_| Value::Null)
                    .map_err(engine);
            }
        }
        let Some(m) = args.get("mask") else { return Err(invalid("`all` and `none` take true")) };
        let r = Rect::from_xywh(
            int(m.get("x").unwrap_or(&Value::Null), "mask.x")? as i32,
            int(m.get("y").unwrap_or(&Value::Null), "mask.y")? as i32,
            int(m.get("width").unwrap_or(&Value::Null), "mask.width")?.clamp(0, 1_000_000) as u32,
            int(m.get("height").unwrap_or(&Value::Null), "mask.height")?.clamp(0, 1_000_000) as u32,
        );
        let bytes = unb64(m.get("data").unwrap_or(&Value::Null), "mask.data")?;
        if r.is_empty() || bytes.len() != r.width() as usize * r.height() as usize {
            return Err(invalid("`mask.data` must hold width × height gray8 bytes"));
        }
        let samples: Vec<f32> = bytes.iter().map(|&b| f32::from(b) / 255.0).collect();
        let mode = mode.to_string();
        self.edit("Plugin Selection", |doc, _| {
            use photocraft_algo::selection as sel;
            let bounds = doc.bounds();
            let mut shape = Surface::new(PixelFormat::GRAY8);
            shape.write_region(r, &samples);
            let shape = sel::combine_surfaces(&shape, &Surface::new(PixelFormat::GRAY8), bounds, |a, _| a);
            let combined = match (mode.as_str(), doc.selection.take()) {
                ("add", Some(o)) => sel::combine_surfaces(&o, &shape, bounds, |a, b| a.max(b)),
                ("subtract", Some(o)) => sel::combine_surfaces(&o, &shape, bounds, |a, b| a * (1.0 - b)),
                ("intersect", Some(o)) => sel::combine_surfaces(&o, &shape, bounds, |a, b| a.min(b)),
                ("subtract" | "intersect", None) => Surface::new(PixelFormat::GRAY8),
                _ => shape,
            };
            let empty = combined.content_bounds().intersect(&bounds).is_empty();
            doc.selection = (!empty).then_some(combined);
            Ok(())
        })
        .map_err(engine)?;
        Ok(Value::Null)
    }

    /// `files.pick`: validated now, asked when the event commits.
    fn plugin_pick(&mut self, args: Value, cx: &mut OpCtx<'_>) -> Result<Value, OpError> {
        let mode = args.get("mode").and_then(Value::as_str).and_then(PickMode::parse).ok_or_else(|| invalid("`mode` is folder, save or open"))?;
        let text = |key: &str, max: usize| -> Result<String, OpError> {
            match args.get(key) {
                None | Some(Value::Null) => Ok(String::new()),
                Some(Value::String(s)) if s.chars().count() <= max && !s.chars().any(char::is_control) => Ok(s.clone()),
                _ => Err(invalid(format!("`{key}` must be text of at most {max} characters"))),
            }
        };
        let title = text("title", 200)?;
        let name = text("name", 255)?;
        if !name.is_empty() {
            safe_file_name(&name)?;
        }
        if cx.staged.picks.len() >= 4 {
            return Err(budget("At most four file questions per event"));
        }
        let request = self.plugin_picks.request_id();
        cx.staged.picks.push(PickRequest { request, owner: cx.owner.to_string(), plugin: cx.plugin.to_string(), mode, title, name });
        Ok(json!({"request": request}))
    }

    /// The next file question for the desktop's dialog.
    pub fn take_plugin_pick(&mut self) -> Option<PickRequest> {
        self.plugin_picks.requests.pop_front()
    }

    /// Answers a file question: `None` is Cancel. The plugin gets a `files.picked` event.
    pub fn answer_plugin_pick(&mut self, req: &PickRequest, path: Option<PathBuf>) {
        let event = match path {
            Some(path) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let handle = self.plugin_picks.grant(&req.plugin, path, req.mode);
                json!({"event": "files.picked", "request": req.request, "handle": handle, "name": name})
            }
            None => json!({"event": "files.picked", "request": req.request, "cancelled": true}),
        };
        self.plugin_followups.push_back((req.owner.clone(), event));
    }

    /// Asks (or answers headlessly) the picks an event staged, after it committed.
    pub(crate) fn plugin_commit_picks(&mut self, picks: Vec<PickRequest>) -> Vec<String> {
        let mut errors = vec![];
        for req in picks {
            if let Some(path) = self.plugin_picks.answers.pop_front() {
                let ok = match req.mode {
                    PickMode::Folder => path.is_dir(),
                    PickMode::Open => path.is_file(),
                    PickMode::Save => path.parent().is_none_or(|p| p.as_os_str().is_empty() || p.is_dir()) && !path.is_dir(),
                };
                if ok {
                    self.answer_plugin_pick(&req, Some(path));
                } else {
                    errors.push(format!("--pick {} does not fit a {:?} question", path.display(), req.mode));
                    self.answer_plugin_pick(&req, None);
                }
            } else if self.plugin_picks.interactive {
                self.plugin_picks.requests.push_back(req);
            } else {
                errors.push("No file picker here: pass `--pick <PATH>` to photocraft-cli run".to_string());
                self.answer_plugin_pick(&req, None);
            }
        }
        errors
    }
}

/// `adjustment.apply` as an engine command and its checked parameters.
fn adjustment_command(args: &Value) -> Result<(String, Value), OpError> {
    let name = args.get("adjustment").and_then(Value::as_str).ok_or_else(|| invalid("`adjustment` is missing"))?;
    let (_, layer_ok) =
        ADJUSTMENTS.iter().find(|(n, _)| *n == name).ok_or_else(|| OpError::denied(format!("Adjustment `{name}` is not available to plugins")))?;
    let as_layer = args.get("as_layer").and_then(Value::as_bool).unwrap_or(false);
    if as_layer && !layer_ok {
        return Err(invalid(format!("`{name}` has no adjustment layer")));
    }
    let params = args.get("params").cloned().unwrap_or_else(|| json!({}));
    let map = params.as_object().ok_or_else(|| invalid("`params` must be an object"))?;
    if serde_json::to_vec(&params).map_err(engine)?.len() > 16 * 1024 {
        return Err(budget("`params` is larger than 16 KiB"));
    }
    if let Some(k) = map.keys().find(|k| k.starts_with("__") || ADJUSTMENT_RESERVED.contains(&k.as_str())) {
        return Err(invalid(format!("`{k}` is not available to plugins")));
    }
    let cmd = if as_layer { format!("layer.newAdjustmentLayer.{name}") } else { format!("image.adjustments.{name}") };
    Ok((cmd, params))
}

/// 256-bin histograms of straight RGBA, weighted by alpha > 0 and the optional mask.
struct Histogram {
    bins: [[u64; 256]; 5],
    count: u64,
}
impl Histogram {
    fn new() -> Self {
        Self { bins: [[0; 256]; 5], count: 0 }
    }
    fn add(&mut self, rgba: &[f32], mask: Option<&[f32]>) {
        let Self { bins, count } = self;
        let bin = |v: f32| (if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 } * 255.0).round() as usize;
        for (i, px) in rgba.chunks_exact(4).enumerate() {
            if px[3] <= 0.0 || mask.is_some_and(|m| m.get(i).copied().unwrap_or(0.0) < 0.5) {
                continue;
            }
            *count += 1;
            let lum = 0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2];
            for (c, v) in [px[0], px[1], px[2], px[3], lum].into_iter().enumerate() {
                if let Some(b) = bins.get_mut(c).and_then(|b| b.get_mut(bin(v).min(255))) {
                    *b += 1;
                }
            }
        }
    }
    fn finish(self) -> Value {
        let Self { bins, count } = self;
        let mean = |b: &[u64; 256]| if count == 0 { 0.0 } else { b.iter().enumerate().map(|(i, n)| i as f64 * *n as f64).sum::<f64>() / count as f64 };
        let round = |v: f64| (v * 100.0).round() / 100.0;
        json!({
            "count": count,
            "red": bins[0].to_vec(), "green": bins[1].to_vec(), "blue": bins[2].to_vec(), "alpha": bins[3].to_vec(), "luminosity": bins[4].to_vec(),
            "mean": {"red": round(mean(&bins[0])), "green": round(mean(&bins[1])), "blue": round(mean(&bins[2])), "luminosity": round(mean(&bins[4]))},
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_plain_and_never_executable() {
        for good in ["Sky.png", "layers.json", "a b.tif"] {
            assert!(safe_file_name(good).is_ok(), "{good}");
        }
        for bad in ["", ".hidden", "../x.png", "a/b.png", "a\\b.png", "run.sh", "x.EXE", "trail.", "nul\0.png", "C:x.png"] {
            assert!(safe_file_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn save_handles_refuse_executable_names() {
        for name in ["run.sh", "Launch.desktop", ".profile", "bad.EXE"] {
            let picked = Picked { plugin: "test".into(), path: PathBuf::from(name), mode: PickMode::Save };
            assert!(target(&picked, &json!({})).is_err(), "{name}");
        }
    }

    #[test]
    fn file_commit_reports_partial_publication_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.txt");
        let last = dir.path().join("last.txt");
        let mut staged = Staged::default();
        staged.write(first.clone(), b"published".to_vec()).unwrap();
        staged.write(dir.path().join("missing/fail.txt"), vec![]).unwrap();
        staged.write(last.clone(), vec![]).unwrap();
        let error = staged.publish_files().unwrap_err();
        assert!(error.message.contains("1 file(s) already published"), "{}", error.message);
        assert_eq!(std::fs::read(first).unwrap(), b"published");
        assert!(!last.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_planted_after_staging_cannot_redirect_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, b"keep").unwrap();
        let folder = dir.path().join("picked");
        std::fs::create_dir(&folder).unwrap();
        let picked = Picked { plugin: "test".into(), path: folder.clone(), mode: PickMode::Folder };
        let path = target(&picked, &json!({"name":"out.txt"})).unwrap();
        let mut staged = Staged::default();
        staged.write(path.clone(), b"new".to_vec()).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        staged.publish_files().unwrap();
        assert_eq!(std::fs::read(outside).unwrap(), b"keep");
        assert_eq!(std::fs::read(path).unwrap(), b"new");
    }

    #[test]
    fn adjustments_are_allowlisted_and_params_checked() {
        assert_eq!(adjustment_command(&json!({"adjustment":"invert"})).unwrap().0, "image.adjustments.invert");
        assert_eq!(adjustment_command(&json!({"adjustment":"levels","as_layer":true})).unwrap().0, "layer.newAdjustmentLayer.levels");
        assert_eq!(adjustment_command(&json!({"adjustment":"colorLookup"})).unwrap_err().code, Code::Denied);
        assert!(adjustment_command(&json!({"adjustment":"equalize","as_layer":true})).is_err());
        for key in ["target", "eyedropper", "__x", "layer"] {
            assert!(adjustment_command(&json!({"adjustment":"levels","params":{key: 1}})).is_err(), "{key}");
        }
    }

    #[test]
    fn pixel_formats_round_trip_and_reject_bad_floats() {
        let px = [0.0, 0.5, 1.0, 1.0];
        for f in [Format::Rgba8, Format::Rgba16, Format::Rgba32f] {
            let back = f.decode(&f.encode(&px)).unwrap();
            assert!(back.iter().zip(px).all(|(a, b)| (a - b).abs() < 0.003), "{}", f.name());
        }
        assert!(Format::Rgba32f.decode(&f32::NAN.to_le_bytes().repeat(4)).is_err());
    }

    #[test]
    fn histogram_counts_opaque_pixels_inside_the_mask() {
        let rgba = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.5, 0.5, 0.5, 0.0];
        let mut h = Histogram::new();
        h.add(&rgba, None);
        let h = h.finish();
        assert_eq!(h["count"], 2);
        assert_eq!(h["red"][255], 1);
        assert_eq!(h["blue"][255], 1);
        let mut h = Histogram::new();
        h.add(&rgba, Some(&[1.0, 0.0, 1.0]));
        let h = h.finish();
        assert_eq!(h["count"], 1);
    }
}
