//! Pixel and parameter conversion for `raster.filter@1` effects.
//!
//! The document side reads straight (unpremultiplied) RGB or RGBA samples as normalised `f32`
//! at any storage depth; plugins receive premultiplied RGBA `f32` in the working space.
pub use craft_ext_core::manifest::{Param, ParamKind};
use serde_json::{Map, Value, json};

/// Parameter ids the host's command layer reads itself; a contribution declaring one cannot be
/// offered as a command without its parameter being taken for the host's meaning.
pub const RESERVED_PARAMS: &[&str] = &["layer", "target", "coalesce"];
const MAX_TEXT: usize = 4096;
const POINT_RANGE: f64 = 100_000.0;

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

/// The command layer's parameter notation for `params` (shown to agents and read by the
/// generated dialog). Points are two fields, `<id>.x` and `<id>.y`.
pub fn params_doc(params: &[Param]) -> String {
    let mut parts = Vec::new();
    for p in params {
        let id = &p.id;
        match &p.kind {
            ParamKind::Slider { default, min, max, .. } => parts.push(format!("\"{id}\":{min}..{max}={default}")),
            ParamKind::Angle { default } => parts.push(format!("\"{id}\":-360..360={}", default.clamp(-360.0, 360.0))),
            ParamKind::Checkbox { default } => parts.push(format!("\"{id}\":bool={default}")),
            ParamKind::Select { options, default } => {
                let names: Vec<String> = options.iter().map(|o| option_name(o)).collect();
                let def = names.get(*default as usize).cloned().unwrap_or_default();
                parts.push(format!("\"{id}\":\"{}\"=\"{def}\"", names.join("|")));
            }
            ParamKind::Color { default } => parts.push(format!("\"{id}\":color={}", hex(default))),
            ParamKind::Point { default: [x, y] } => {
                parts.push(format!("\"{id}.x\":-{POINT_RANGE}..{POINT_RANGE}={x}"));
                parts.push(format!("\"{id}.y\":-{POINT_RANGE}..{POINT_RANGE}={y}"));
            }
            ParamKind::Text { .. } => parts.push(format!("\"{id}\":text")),
        }
    }
    format!("{{{}}} (select: option or index; color: [r,g,b,a] or #rrggbb; point: [x,y])", parts.join(","))
}

/// An option as the notation can carry it (no quote or separator characters).
fn option_name(o: &str) -> String {
    o.replace('"', "'").replace('|', "/")
}

fn hex(&[r, g, b, _]: &[f64; 4]) -> String {
    let q = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", q(r), q(g), q(b))
}

fn parse_hex(s: &str, alpha: f64) -> Option<[f64; 4]> {
    let h = s.strip_prefix('#')?;
    if !(h.len() == 6 || h.len() == 8) || !h.is_ascii() {
        return None;
    }
    let byte = |i: usize| h.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok()).map(|v| f64::from(v) / 255.0);
    Some([byte(0)?, byte(2)?, byte(4)?, if h.len() == 8 { byte(6)? } else { alpha }])
}

fn numbers<const N: usize>(v: &Value) -> Option<[f64; N]> {
    let a = v.as_array().filter(|a| a.len() == N)?;
    let mut out = [0.0; N];
    for (slot, item) in out.iter_mut().zip(a) {
        *slot = item.as_f64().filter(|x| x.is_finite())?;
    }
    Some(out)
}

/// Every declared parameter, from `given` or its default, in the form the guest SDK reads
/// (select = option index, color = `[r,g,b,a]`, point = `[x,y]`). Unknown or invalid values are
/// errors naming the parameter, so nothing runs with a value the plugin did not declare.
pub fn resolve_params(params: &[Param], given: &Value) -> Result<Value, String> {
    let empty = Map::new();
    let given = match given {
        Value::Object(m) => m,
        Value::Null => &empty,
        _ => return Err("params must be a JSON object".into()),
    };
    for key in given.keys() {
        let known = params.iter().any(|p| {
            *key == p.id || (matches!(p.kind, ParamKind::Point { .. }) && (*key == format!("{}.x", p.id) || *key == format!("{}.y", p.id)))
        });
        if !known {
            let ids: Vec<&str> = params.iter().map(|p| p.id.as_str()).collect();
            return Err(format!("unknown parameter `{key}` (expected: {})", ids.join(", ")));
        }
    }
    let mut out = Map::new();
    for p in params {
        let bad = |what: &str| format!("parameter `{}` {what}", p.id);
        let v = given.get(&p.id);
        let value = match &p.kind {
            ParamKind::Slider { default, min, max, integer } => {
                let x = match v {
                    None => *default,
                    Some(v) => v.as_f64().filter(|x| x.is_finite()).ok_or_else(|| bad("must be a number"))?,
                };
                if x < *min || x > *max {
                    return Err(bad(&format!("must be between {min} and {max}")));
                }
                if *integer { json!(x.round() as i64) } else { json!(x) }
            }
            ParamKind::Angle { default } => json!(match v {
                None => *default,
                Some(v) => v.as_f64().filter(|x| x.is_finite()).ok_or_else(|| bad("must be a number of degrees"))?,
            }),
            ParamKind::Checkbox { default } => json!(match v {
                None => *default,
                Some(v) => v.as_bool().ok_or_else(|| bad("must be true or false"))?,
            }),
            ParamKind::Select { options, default } => {
                let index = match v {
                    None => u64::from(*default),
                    Some(Value::String(s)) => options.iter().position(|o| o == s || option_name(o) == *s).ok_or_else(|| bad(&format!("must be one of: {}", options.join(", "))))? as u64,
                    Some(v) => v.as_u64().ok_or_else(|| bad("must be an option name or index"))?,
                };
                if index >= options.len() as u64 {
                    return Err(bad("is not one of its options"));
                }
                json!(index)
            }
            ParamKind::Color { default } => {
                let c = match v {
                    None => *default,
                    Some(Value::String(s)) => parse_hex(s, default.get(3).copied().unwrap_or(1.0)).ok_or_else(|| bad("must be #rrggbb, #rrggbbaa or [r,g,b,a]"))?,
                    Some(v) => numbers::<4>(v).or_else(|| numbers::<3>(v).map(|[r, g, b]| [r, g, b, 1.0])).ok_or_else(|| bad("must be [r,g,b,a]"))?,
                };
                if c.iter().any(|x| !(0.0..=1.0).contains(x)) {
                    return Err(bad("components must be between 0 and 1"));
                }
                json!(c)
            }
            ParamKind::Point { default } => {
                let mut pt = match v {
                    None => *default,
                    Some(v) => numbers::<2>(v).ok_or_else(|| bad("must be [x, y]"))?,
                };
                for (i, axis) in ["x", "y"].iter().enumerate() {
                    if let Some(c) = given.get(&format!("{}.{axis}", p.id)) {
                        let c = c.as_f64().filter(|x| x.is_finite()).ok_or_else(|| bad("must be [x, y]"))?;
                        if let Some(slot) = pt.get_mut(i) {
                            *slot = c;
                        }
                    }
                }
                json!(pt)
            }
            ParamKind::Text { default } => {
                let s = match v {
                    None => default.clone(),
                    Some(v) => v.as_str().ok_or_else(|| bad("must be text"))?.to_owned(),
                };
                if s.len() > MAX_TEXT {
                    return Err(bad("is too long"));
                }
                json!(s)
            }
        };
        out.insert(p.id.clone(), value);
    }
    Ok(Value::Object(out))
}

/// `values` (as [`resolve_params`] returns them) in the generated dialog's field form: a select
/// as its option name, a colour as `#rrggbb`, a point as `<id>.x` / `<id>.y`.
pub fn dialog_fields(params: &[Param], values: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    for p in params {
        let Some(v) = values.get(&p.id) else { continue };
        match &p.kind {
            ParamKind::Select { options, .. } => {
                if let Some(o) = v.as_u64().and_then(|i| options.get(i as usize)) {
                    out.insert(p.id.clone(), json!(option_name(o)));
                }
            }
            ParamKind::Color { .. } => {
                if let Some(c) = numbers::<4>(v) {
                    out.insert(p.id.clone(), json!(hex(&c)));
                }
            }
            ParamKind::Point { .. } => {
                if let Some([x, y]) = numbers::<2>(v) {
                    out.insert(format!("{}.x", p.id), json!(x));
                    out.insert(format!("{}.y", p.id), json!(y));
                }
            }
            _ => {
                out.insert(p.id.clone(), v.clone());
            }
        }
    }
    out
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
    fn params_resolve_to_the_guest_form() {
        let params: Vec<Param> = serde_json::from_value(json!([
            {"id":"size","label":"Size","type":"slider","default":4.0,"min":1.0,"max":10.0,"integer":true},
            {"id":"mode","label":"Mode","type":"select","options":["Dots","Lines"],"default":0},
            {"id":"tint","label":"Tint","type":"color","default":[1.0,1.0,1.0,0.5]},
            {"id":"center","label":"Center","type":"point"}
        ]))
        .unwrap();
        let v = resolve_params(&params, &json!({"size":6.4,"mode":"Lines","tint":"#ff0000","center.x":3})).unwrap();
        assert_eq!(v, json!({"size":6,"mode":1,"tint":[1.0,0.0,0.0,0.5],"center":[3.0,0.0]}));
        assert!(resolve_params(&params, &json!({"size":11})).is_err());
        assert!(resolve_params(&params, &json!({"other":1})).is_err());
        let f = dialog_fields(&params, &v);
        assert_eq!(f.get("mode"), Some(&json!("Lines")));
        assert_eq!(f.get("center.x"), Some(&json!(3.0)));
    }
}
