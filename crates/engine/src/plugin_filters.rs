//! Enabled plugin effects as filter commands: `plugin.<plugin-id>/<contribution>`, in
//! Filter › Plugins › <category>. The command list follows the plugin store: it is rebuilt when
//! the store's generation changes (checked at most every two seconds, the LUT scan cadence).
//!
//! A run converts the active RGB layer to premultiplied RGBA `f32` in the working space, runs
//! the sandboxed transform on the job's worker and writes the result back through the selection
//! as one history step. Any failure leaves the document unchanged.

use serde_json::{Map, Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

/// Why a plugin filter is unavailable in a non-RGB document (also its menu tooltip).
pub const RGB_ONLY: &str = "This filter works on RGB images";
/// The command id prefix of plugin filters.
pub const PREFIX: &str = "plugin.";

/// The private run parameter naming the command a dynamic spec runs (see `inject_kind`).
pub(crate) const COMMAND_PARAM: &str = "__plugin";

/// Whether the menus' tooltip for disabled `id` is [`RGB_ONLY`].
pub fn rgb_only(s: &Session, id: &str) -> bool {
    id.starts_with(PREFIX) && s.active().is_some_and(|d| d.doc.mode != photocraft_color::ColorMode::Rgb)
}

fn enabled(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    if d.doc.mode != photocraft_color::ColorMode::Rgb {
        return Err(RGB_ONLY.into());
    }
    if crate::channel_cmds::edits_channel(s) {
        return Err("plugin filters apply to a layer's colour: target the composite channel".into());
    }
    let id = d.active_layer.ok_or("no active layer")?;
    match &d.doc.layer(id).ok_or("no active layer")?.content {
        photocraft_doc::LayerContent::Raster(_) => Ok(()),
        other => Err(format!("plugin filters need a pixel layer (active layer is {} {} layer)", other.article(), other.kind_name())),
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod registry {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, OnceLock, PoisonError};
    use std::time::{Duration, Instant};

    use photocraft_plugins::store::FilterSource;

    use super::*;

    pub(super) struct Entry {
        pub spec: &'static CommandSpec,
        pub source: FilterSource,
        pub plugin_name: String,
    }

    #[derive(Default)]
    struct Registry {
        checked: Option<Instant>,
        generation: Option<u64>,
        entries: Vec<Entry>,
        /// Specs are `'static`: each distinct (id, label, menu, params) is leaked once and reused,
        /// so refreshes only allocate for contributions that actually changed.
        interned: HashMap<String, &'static CommandSpec>,
    }

    static REVISION: AtomicU64 = AtomicU64::new(0);

    fn registry() -> &'static Mutex<Registry> {
        static R: OnceLock<Mutex<Registry>> = OnceLock::new();
        R.get_or_init(Mutex::default)
    }

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    fn spec_for(r: &mut Registry, source: &FilterSource) -> Option<&'static CommandSpec> {
        let c = &source.contribution;
        // The host reads these keys itself; such a parameter could never reach the plugin.
        if c.params.iter().any(|p| photocraft_plugins::filter::RESERVED_PARAMS.contains(&p.id.as_str())) {
            return None;
        }
        let id = format!("{PREFIX}{}/{}", source.plugin, c.id);
        let label = if c.params.is_empty() { c.label.clone() } else { format!("{}…", c.label) };
        let category = c.category.clone().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "Other".into());
        let params = photocraft_plugins::filter::params_doc(&c.params);
        let key = format!("{id}\u{0}{label}\u{0}{category}\u{0}{params}");
        if let Some(spec) = r.interned.get(&key) {
            return Some(spec);
        }
        let menu: &'static [&'static str] = Box::leak(vec!["Filter", "Plugins", leak(category)].into_boxed_slice());
        let spec: &'static CommandSpec = Box::leak(Box::new(CommandSpec {
            id: leak(id),
            label: leak(label),
            menu,
            shortcut: None,
            params: leak(params),
            enabled,
            run: super::run,
            journal: true,
        }));
        r.interned.insert(key, spec);
        Some(spec)
    }

    /// Rebuild from the store when its generation changed. Store errors keep the last list:
    /// a busy or half-written store must not make filters flicker out of the menus.
    fn refresh(r: &mut Registry) {
        if r.checked.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            return;
        }
        r.checked = Some(Instant::now());
        let generation = if photocraft_plugins::safe_mode() { Some(0) } else { photocraft_plugins::store::generation().ok() };
        if generation.is_none() || generation == r.generation {
            return;
        }
        let Ok(found) = photocraft_plugins::store::named_filters() else { return };
        r.generation = generation;
        let mut entries = Vec::new();
        for (source, plugin_name) in found {
            if let Some(spec) = spec_for(r, &source) {
                entries.push(Entry { spec, source, plugin_name });
            }
        }
        r.entries = entries;
        REVISION.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn with<T>(f: impl FnOnce(&[Entry]) -> T) -> T {
        let mut r = registry().lock().unwrap_or_else(PoisonError::into_inner);
        refresh(&mut r);
        f(&r.entries)
    }

    pub(super) fn revision() -> u64 {
        with(|_| ());
        REVISION.load(Ordering::Relaxed)
    }
}

/// The spec of plugin filter command `id`, while its contribution is enabled.
pub fn find(id: &str) -> Option<&'static CommandSpec> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if !id.starts_with(PREFIX) {
            return None;
        }
        registry::with(|entries| entries.iter().find(|e| e.spec.id == id).map(|e| e.spec))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = id;
        None
    }
}

/// Every enabled plugin filter command, in store order.
pub fn specs() -> Vec<&'static CommandSpec> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        registry::with(|entries| entries.iter().map(|e| e.spec).collect())
    }
    #[cfg(target_arch = "wasm32")]
    Vec::new()
}

/// Changes whenever the plugin filter list does (for menu caches).
pub fn revision() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        registry::revision()
    }
    #[cfg(target_arch = "wasm32")]
    0
}

/// The generated dialog's extra fields for plugin filter `id`: the manifest's labels, which
/// fields are angles, text defaults, and the plugin presets for this filter as
/// `[{id, label, fields}]`. `None` for other commands.
pub fn dialog_fields(id: &str) -> Option<Map<String, Value>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use photocraft_plugins::filter::{ParamKind, dialog_fields, resolve_params};
        let contribution = registry::with(|entries| entries.iter().find(|e| e.spec.id == id).map(|e| (e.source.contribution.clone(), e.source.plugin.to_string())))?;
        let (c, plugin) = contribution;
        let mut labels = Map::new();
        let mut angles = Vec::new();
        for p in &c.params {
            match p.kind {
                ParamKind::Point { .. } => {
                    labels.insert(format!("{}.x", p.id), json!(format!("{} X", p.label)));
                    labels.insert(format!("{}.y", p.id), json!(format!("{} Y", p.label)));
                }
                ParamKind::Angle { .. } => {
                    angles.push(json!(p.id));
                    labels.insert(p.id.clone(), json!(p.label));
                }
                _ => {
                    labels.insert(p.id.clone(), json!(p.label));
                }
            }
        }
        let mut out = dialog_fields(&c.params, &resolve_params(&c.params, &Value::Null).unwrap_or(Value::Null));
        out.insert("__labels".into(), Value::Object(labels));
        out.insert("__angles".into(), Value::Array(angles));
        // A preset names its effect as `<plugin>/<contribution>`; one that no longer fits the
        // filter's parameters is left out rather than shown and then refused.
        let effect = format!("{plugin}/{}", c.id);
        let presets: Vec<Value> = photocraft_plugins::resources::presets(&effect)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|p| {
                let values = resolve_params(&c.params, &p.params).ok()?;
                Some(json!({"id": p.id, "label": p.label, "fields": dialog_fields(&c.params, &values)}))
            })
            .collect();
        if !presets.is_empty() {
            out.insert("__presets".into(), Value::Array(presets));
        }
        Some(out)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = id;
        None
    }
}

/// Run plugin filter `__plugin` on the active (or named) layer.
fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let id = p.get(COMMAND_PARAM).and_then(Value::as_str).unwrap_or_default().to_string();
    run_plugin(s, &id, p)
}

#[cfg(target_arch = "wasm32")]
fn run_plugin(_: &mut Session, id: &str, _: &Value) -> Result<Value> {
    Err(EngineError::UnknownCommand(id.to_string()))
}

#[cfg(not(target_arch = "wasm32"))]
fn run_plugin(s: &mut Session, id: &str, p: &Value) -> Result<Value> {
    use photocraft_color::{ColorMode, SampleType};
    use photocraft_doc::LayerContent;
    use photocraft_plugins::filter::{from_premultiplied, resolve_params, to_premultiplied};

    let entry = registry::with(|entries| entries.iter().find(|e| e.spec.id == id).map(|e| (e.source.clone(), e.plugin_name.clone())));
    let (source, plugin_name) = entry.ok_or_else(|| EngineError::UnknownCommand(id.to_string()))?;
    let mut given = p.clone();
    if let Value::Object(m) = &mut given {
        m.retain(|k, _| !k.starts_with("__") && !photocraft_plugins::filter::RESERVED_PARAMS.contains(&k.as_str()));
    }
    let params = resolve_params(&source.contribution.params, &given).map_err(|msg| EngineError::BadParams { cmd: id.to_string(), msg })?;
    let layer = crate::commands::layer_param(s, p)?;
    let label = format!("{} · {plugin_name}", source.contribution.label);
    let failed = label.clone();
    let id = id.to_string();
    crate::jobs::edit_job(
        s,
        &label,
        move |doc, _, ctx| {
            ctx.check()?;
            if doc.mode != ColorMode::Rgb {
                return Err(EngineError::Other(RGB_ONLY.into()));
            }
            // Plugins see a 32-bit document's linear light, as its pixels store it; integer
            // depths keep the document's encoded values.
            let color = if doc.depth == SampleType::F32 { "linear-srgb" } else { "srgb" };
            let area = doc.bounds();
            let selection = doc.selection.clone();
            let locks = doc.effective_locks(layer);
            let l = doc.layer_mut(layer).ok_or(EngineError::NoLayer(layer))?;
            if locks.pixels || locks.all {
                return Err(EngineError::Other(format!("Could not complete your request because the layer \"{}\" is locked", l.name)));
            }
            let LayerContent::Raster(surf) = &mut l.content else {
                return Err(EngineError::Other("plugin filters need a pixel layer".into()));
            };
            let format = surf.format();
            let n = surf.channels();
            if format.mode != ColorMode::Rgb || area.is_empty() {
                return Err(EngineError::Other(RGB_ONLY.into()));
            }
            let (w, h) = (area.width(), area.height());
            // Source, premultiplied buffer, plugin output and the converted result.
            let bytes = u64::from(w) * u64::from(h) * 4 * (4 + 4 + 4 + n as u64);
            let _working = photocraft_raster::memory::try_reserve_operation(bytes).map_err(EngineError::Other)?;
            let before = surf.read_region(area);
            let px = to_premultiplied(&before, n).map_err(EngineError::Other)?;
            let cancelled = || ctx.cancelled();
            let out = source.run(&px, w, h, color, params, &cancelled).map_err(|e| {
                if ctx.cancelled() { EngineError::Cancelled } else { EngineError::Other(format!("{failed} could not run: {e}")) }
            })?;
            ctx.check()?;
            let mut after = from_premultiplied(&out, n, format.sample != SampleType::F32).map_err(EngineError::Other)?;
            if after.len() != before.len() {
                return Err(EngineError::Other(format!("{failed} returned an image of the wrong size")));
            }
            // The selection blends the result in by its coverage, as built-in filters do; a
            // layer with locked transparency keeps its alpha.
            let mask = selection.as_ref().map(|sel| sel.read_region(area));
            for (i, (new, old)) in after.chunks_exact_mut(n).zip(before.chunks_exact(n)).enumerate() {
                let t = mask.as_ref().map_or(1.0, |m| m.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0));
                for (c, (a, b)) in new.iter_mut().zip(old).enumerate() {
                    let keep = locks.transparency && format.alpha && c + 1 == n;
                    *a = if keep { *b } else { b + (*a - b) * t };
                }
            }
            surf.write_region(area, &after);
            surf.prune();
            Ok(())
        },
        move |()| json!({ "layer": layer.0, "filter": id }),
    )
}
