//! Pixel and parameter conversion for `raster.filter@1` effects.
//!
//! The document side reads straight (unpremultiplied) RGB or RGBA samples as normalised `f32`
//! at any storage depth; plugins receive premultiplied RGBA `f32` in the working space.
pub use craft_ext_core::form::{Param, ParamKind};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

/// Parameter ids the host's command layer reads itself; a contribution declaring one cannot be
/// offered as a command without its parameter being taken for the host's meaning.
pub const RESERVED_PARAMS: &[&str] = &["layer", "target", "coalesce"];

/// Straight RGB (`channels == 3`) or RGBA (`channels == 4`) samples to premultiplied RGBA.
pub fn to_premultiplied(src: &[f32], channels: usize) -> Result<Vec<f32>, String> {
    if !(channels == 3 || channels == 4) || src.len() % channels != 0 {
        return Err("This filter works on RGB images".into());
    }
    let mut out = Vec::new();
    out.try_reserve_exact(src.len() / channels * 4).map_err(|_| "Not enough memory for this filter".to_string())?;
    for px in src.chunks_exact(channels) {
        let (r, g, b) = (px.first().copied().unwrap_or(0.0), px.get(1).copied().unwrap_or(0.0), px.get(2).copied().unwrap_or(0.0));
        let a = if channels == 4 { px.get(3).copied().unwrap_or(1.0).clamp(0.0, 1.0) } else { 1.0 };
        out.extend_from_slice(&[r * a, g * a, b * a, a]);
    }
    Ok(out)
}

/// Premultiplied RGBA back to straight samples with `channels` (3 or 4) per pixel. Colour is
/// clamped to 0..=1 only for integer depths; float documents keep HDR values. Alpha is always
/// 0..=1, and a fully transparent pixel has no colour.
pub fn from_premultiplied(px: &[f32], channels: usize, integer: bool) -> Result<Vec<f32>, String> {
    if !(channels == 3 || channels == 4) || px.len() % 4 != 0 {
        return Err("This filter works on RGB images".into());
    }
    let mut out = Vec::new();
    out.try_reserve_exact(px.len() / 4 * channels).map_err(|_| "Not enough memory for this filter".to_string())?;
    for p in px.chunks_exact(4) {
        let a = p.get(3).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        for c in p.iter().take(3) {
            let v = if a > 0.0 { c / a } else { 0.0 };
            let v = if v.is_finite() { v } else { 0.0 };
            out.push(if integer { v.clamp(0.0, 1.0) } else { v });
        }
        if channels == 4 {
            out.push(a);
        }
    }
    Ok(out)
}

/// Command documentation carries the canonical form schema; JSON parameters are Values.
pub fn params_doc(form: &craft_ext_core::form::Form) -> String {
    serde_json::to_string(form).unwrap_or_default()
}

pub fn resolve_params(form: &craft_ext_core::form::Form, given: &Value) -> Result<Value, String> {
    let given: craft_ext_core::form::Values =
        if given.is_null() { Default::default() } else { serde_json::from_value(given.clone()).map_err(|e| e.to_string())? };
    form.validate_values(&given)?;
    let mut values = form.defaults();
    values.extend(given);
    serde_json::to_value(values).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quantize8(v: f32) -> f32 {
        (v.clamp(0.0, 1.0) * 255.0).round() / 255.0
    }

    #[test]
    fn eight_bit_round_trip_is_exact_for_visible_pixels() {
        let mut src = Vec::new();
        for a in [1u8, 2, 17, 128, 254, 255] {
            for c in [0u8, 1, 37, 128, 200, 255] {
                src.extend([c, 255 - c, c / 2, a].map(|v| f32::from(v) / 255.0));
            }
        }
        let back = from_premultiplied(&to_premultiplied(&src, 4).unwrap(), 4, true).unwrap();
        assert_eq!(back.iter().map(|v| quantize8(*v)).collect::<Vec<_>>(), src);
        // Opaque RGB without alpha keeps three channels.
        let rgb = [0.2f32, 0.4, 1.0];
        let pre = to_premultiplied(&rgb, 3).unwrap();
        assert_eq!(pre, vec![0.2, 0.4, 1.0, 1.0]);
        assert_eq!(from_premultiplied(&pre, 3, true).unwrap(), rgb);
    }

    #[test]
    fn float_round_trip_keeps_hdr_and_integer_depths_clamp() {
        let src = [4.5f32, -0.25, 0.125, 0.5, 0.75, 0.0, 1.5, 1.0];
        let pre = to_premultiplied(&src, 4).unwrap();
        let back = from_premultiplied(&pre, 4, false).unwrap();
        for (a, b) in back.iter().zip(src) {
            assert!((a - b).abs() < 1e-6, "{a} vs {b}");
        }
        assert_eq!(from_premultiplied(&pre, 4, true).unwrap(), vec![1.0, 0.0, 0.125, 0.5, 0.75, 0.0, 1.0, 1.0]);
        // A transparent pixel has no colour, and non-finite output never reaches the document.
        assert_eq!(from_premultiplied(&[1.0, f32::NAN, 0.0, 0.0], 4, false).unwrap(), vec![0.0; 4]);
        assert!(to_premultiplied(&[0.0; 2], 2).is_err());
    }

    #[test]
    fn values_keep_ids_rgba_and_point_space() {
        let form: craft_ext_core::form::Form = serde_json::from_value(json!({"schema":1,"key":"f","title":"Filter","params":[
            {"key":"mode","label":"Mode","kind":{"type":"choice","options":[{"id":"dots","label":"Dots"}],"default":"dots"}},
            {"key":"color","label":"Color","kind":{"type":"color","default":[2,0,0,0.5],"alpha":true}},
            {"key":"point","label":"Point","kind":{"type":"point","default":[20,30],"space":"image-percent"}}
        ]}))
        .unwrap();
        let values = resolve_params(&form, &json!({})).unwrap();
        assert_eq!(values, json!({"mode":"dots","color":[2.0,0.0,0.0,0.5],"point":[20.0,30.0]}));
        for bad in [json!({"mode":0}), json!({"point.x":5}), json!({"color":"#ff0000"}), json!({"unknown":1})] {
            assert!(resolve_params(&form, &bad).is_err());
        }
    }
}

pub fn empty_form(key: &str, title: &str) -> craft_ext_core::form::Form {
    craft_ext_core::form::Form { schema: 1, key: key.into(), title: title.into(), params: vec![], preview: None }
}
