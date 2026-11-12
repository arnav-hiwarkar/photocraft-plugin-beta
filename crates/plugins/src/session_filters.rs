//! The built-in filters a plugin session may run through `filter.apply`, and the checks on the
//! arguments it passes.
//!
//! The list holds pixel-only filters: each takes numbers, choices and flags and reads nothing
//! outside the active layer. Left out: filters that read a file, another document or another
//! channel (Displace's map, Lens Blur's depth channel, Lighting Effects' texture channel, Relight,
//! Picture Frame), that run other commands (Last Filter, Filter Gallery), that change the layer's
//! kind (Convert for Smart Filters), or that open their own tool (Camera Raw, Liquify, Vanishing
//! Point, Lens Correction, Adaptive Wide Angle). Plugin filters are never nested in a session
//! event, and the gallery and blur-gallery filters wait until their inputs are checked the same way.

use serde_json::{Map, Value};

/// The `filter.apply` filters, by command id.
pub const FILTERS: &[&str] = &[
    "filter.blur.average",
    "filter.blur.blur",
    "filter.blur.blurMore",
    "filter.blur.boxBlur",
    "filter.blur.gaussianBlur",
    "filter.blur.motionBlur",
    "filter.blur.radialBlur",
    "filter.blur.smartBlur",
    "filter.blur.surfaceBlur",
    "filter.sharpen.sharpen",
    "filter.sharpen.sharpenEdges",
    "filter.sharpen.sharpenMore",
    "filter.sharpen.smartSharpen",
    "filter.sharpen.unsharpMask",
    "filter.noise.addNoise",
    "filter.noise.despeckle",
    "filter.noise.dustAndScratches",
    "filter.noise.median",
    "filter.noise.reduceNoise",
    "filter.distort.pinch",
    "filter.distort.polarCoordinates",
    "filter.distort.ripple",
    "filter.distort.shear",
    "filter.distort.spherize",
    "filter.distort.twirl",
    "filter.distort.wave",
    "filter.distort.zigZag",
    "filter.pixelate.colorHalftone",
    "filter.pixelate.crystallize",
    "filter.pixelate.facet",
    "filter.pixelate.fragment",
    "filter.pixelate.mezzotint",
    "filter.pixelate.mosaic",
    "filter.pixelate.pointillize",
    "filter.render.clouds",
    "filter.render.differenceClouds",
    "filter.render.fibers",
    "filter.render.flame",
    "filter.render.lensFlare",
    "filter.render.tree",
    "filter.stylize.diffuse",
    "filter.stylize.emboss",
    "filter.stylize.extrude",
    "filter.stylize.findEdges",
    "filter.stylize.oilPaint",
    "filter.stylize.solarize",
    "filter.stylize.tiles",
    "filter.stylize.traceContour",
    "filter.stylize.wind",
    "filter.other.custom",
    "filter.other.highPass",
    "filter.other.hsbHsl",
    "filter.other.maximum",
    "filter.other.minimum",
    "filter.other.offset",
    "filter.video.deInterlace",
    "filter.video.ntscColors",
];

/// Parameter keys a session may not pass: each names another target, or an input read from a file
/// or another document or channel.
const REFUSED_KEYS: &[&str] = &["target", "layer", "mapPath", "mapDocument", "mapLayer", "depthChannel", "depthMap", "texture"];

/// Largest `params` object a `filter.apply` call may carry, serialized.
pub const MAX_PARAMS_BYTES: usize = 16 * 1024;

/// Why a `filter.apply` call is refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The filter is not one plugins may run.
    NotAllowed(String),
    /// The arguments are malformed, or name a target or input a session may not use.
    Invalid(String),
}

/// The command id of `filter` when plugins may run it.
pub fn allowed(filter: &str) -> Option<&'static str> {
    FILTERS.iter().copied().find(|f| *f == filter)
}

/// Checks `filter.apply` arguments `{"filter": id, "params": {…}?}`. Returns the filter's command id
/// and its params as an object (empty when none are given).
pub fn check(args: &Value) -> Result<(&'static str, Value), Refusal> {
    let name = args.get("filter").and_then(Value::as_str).ok_or_else(|| Refusal::Invalid("Invalid filter".into()))?;
    let filter = allowed(name).ok_or_else(|| Refusal::NotAllowed(format!("{name} is not available to plugins")))?;
    let params = match args.get("params") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => {
            let size = serde_json::to_vec(m).map_or(usize::MAX, |b| b.len());
            if size > MAX_PARAMS_BYTES {
                return Err(Refusal::Invalid(format!("params are larger than {MAX_PARAMS_BYTES} bytes")));
            }
            m.clone()
        }
        Some(_) => return Err(Refusal::Invalid("params must be a JSON object".into())),
    };
    if let Some(key) = params.keys().find(|k| k.starts_with("__") || REFUSED_KEYS.contains(&k.as_str())) {
        return Err(Refusal::Invalid(format!("parameter `{key}` is not available to plugins")));
    }
    Ok((filter, Value::Object(params)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn allowlist_is_unique_and_leaves_out_file_document_and_nested_filters() {
        let mut seen = std::collections::BTreeSet::new();
        assert!(FILTERS.iter().all(|f| seen.insert(*f)), "duplicate id in FILTERS");
        for refused in [
            "filter.distort.displace",
            "filter.blur.lensBlur",
            "filter.render.lightingEffects",
            "filter.render.relight",
            "filter.render.pictureFrame",
            "filter.lastFilter",
            "filter.filterGallery",
            "filter.convertForSmartFilters",
            "filter.cameraRaw",
            "filter.liquify",
            "filter.gallery.texturizer",
            "filter.blurGallery.fieldBlur",
            "plugin.someone/effect",
        ] {
            assert!(allowed(refused).is_none(), "{refused} must not be callable from a plugin session");
        }
        assert_eq!(allowed("filter.blur.gaussianBlur"), Some("filter.blur.gaussianBlur"));
    }

    #[test]
    fn refuses_unknown_and_non_string_filters() {
        assert!(matches!(check(&json!({"filter": "filter.distort.displace"})), Err(Refusal::NotAllowed(_))));
        assert!(matches!(check(&json!({"filter": "plugin.x/y"})), Err(Refusal::NotAllowed(_))));
        assert!(matches!(check(&json!({"filter": 3})), Err(Refusal::Invalid(_))));
        assert!(matches!(check(&json!({})), Err(Refusal::Invalid(_))));
        assert!(matches!(check(&json!("filter.blur.blur")), Err(Refusal::Invalid(_))));
    }

    #[test]
    fn accepts_numeric_params_and_defaults_to_none() {
        let (id, params) = check(&json!({"filter": "filter.blur.gaussianBlur", "params": {"radius": 2.5}})).expect("allowed");
        assert_eq!((id, params), ("filter.blur.gaussianBlur", json!({"radius": 2.5})));
        let (_, params) = check(&json!({"filter": "filter.blur.blur"})).expect("allowed");
        assert_eq!(params, json!({}));
        let (_, params) = check(&json!({"filter": "filter.blur.blur", "params": null})).expect("allowed");
        assert_eq!(params, json!({}));
    }

    #[test]
    fn refuses_targets_inputs_and_hidden_keys() {
        for key in ["target", "layer", "mapPath", "mapDocument", "mapLayer", "depthChannel", "depthMap", "texture", "__plugin", "__kind"] {
            let args = json!({"filter": "filter.blur.blur", "params": {key: "x"}});
            assert!(matches!(check(&args), Err(Refusal::Invalid(_))), "{key} must be refused");
        }
    }

    #[test]
    fn refuses_non_object_and_oversized_params() {
        assert!(matches!(check(&json!({"filter": "filter.blur.blur", "params": [1, 2]})), Err(Refusal::Invalid(_))));
        assert!(matches!(check(&json!({"filter": "filter.blur.blur", "params": "radius"})), Err(Refusal::Invalid(_))));
        let big = "x".repeat(MAX_PARAMS_BYTES + 1);
        assert!(matches!(check(&json!({"filter": "filter.blur.blur", "params": {"note": big}})), Err(Refusal::Invalid(_))));
    }
}
