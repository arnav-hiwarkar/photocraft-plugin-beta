//! Enabled, contract-compatible data contributions. Paths are confined to the release.
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Resource {
    pub plugin: String,
    pub id: String,
    pub label: String,
    pub kind: String,
    pub files: Vec<String>,
}

pub fn query() -> Result<Vec<Resource>, Box<dyn std::error::Error>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use craft_ext_core::manifest::ContributionKind;
        let mut out = Vec::new();
        let provided = super::contract()?.provided;
        for (plugin, _, root, manifest, enabled) in super::store::enabled()? {
            let root = root.canonicalize()?;
            for c in manifest.contributions {
                let kind = match c.kind {
                    ContributionKind::Preset => "preset",
                    ContributionKind::Lut => "lut",
                    _ => continue,
                };
                if !enabled.contains(&c.id) || c.contract.check(&provided).is_some() {
                    continue;
                }
                let mut files = Vec::new();
                for file in c.files {
                    let path = root.join(file).canonicalize()?;
                    if !path.starts_with(&root) || !path.is_file() {
                        return Err("The plugin resource is outside its package".into());
                    }
                    let path = path.to_str().ok_or("The plugin resource path is not UTF-8")?;
                    files.push(path.to_owned());
                }
                out.push(Resource { plugin: plugin.to_string(), id: c.id, label: c.label, kind: kind.into(), files });
            }
        }
        Ok(out)
    }
    #[cfg(target_arch = "wasm32")]
    Ok(Vec::new())
}

/// File-based LUT consumers share the same discovery and enablement rules.
pub fn luts() -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    for resource in query()?.into_iter().filter(|r| r.kind == "lut") {
        for path in resource.files {
            out.push((format!("{} · {}", resource.label, resource.plugin), path));
        }
    }
    Ok(out)
}

#[derive(Clone, Debug, Serialize)]
pub struct Preset {
    pub id: String,
    pub label: String,
    pub params: serde_json::Value,
}

/// Return only single-filter presets; a multi-effect recipe cannot honestly be
/// represented by one filter's parameter dialog.
pub fn presets(effect: &str) -> Result<Vec<Preset>, Box<dyn std::error::Error>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::io::Read as _;
        let mut out = Vec::new();
        for resource in query()?.into_iter().filter(|r| r.kind == "preset") {
            for path in resource.files {
                let mut bytes = Vec::new();
                std::fs::File::open(path)?.take(1_048_577).read_to_end(&mut bytes)?;
                if bytes.len() > 1_048_576 {
                    return Err("The plugin preset file is too large".into());
                }
                let data: serde_json::Value = serde_json::from_slice(&bytes)?;
                out.extend(decode_presets(&data, effect));
            }
        }
        Ok(out)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = effect;
        Ok(Vec::new())
    }
}

#[cfg(any(not(target_arch = "wasm32"), test))]
fn decode_presets(data: &serde_json::Value, effect: &str) -> Vec<Preset> {
    data.get("presets").and_then(serde_json::Value::as_array).into_iter().flatten().filter_map(|preset| {
        let effects = preset.get("effects")?.as_array()?;
        if effects.len() != 1 {
            return None;
        }
        let entry = effects.first()?;
        if entry.get("effect")?.as_str()? != effect {
            return None;
        }
        let params = entry.get("params")?.as_object()?;
        Some(Preset {
            id: preset.get("id")?.as_str()?.into(),
            label: preset.get("label")?.as_str()?.into(),
            params: serde_json::Value::Object(params.clone()),
        })
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recipes_are_scoped_to_one_matching_filter() {
        let item = json!({"id":"print", "label":"Print", "effects":[{"effect":"example/filter", "params":{"size":6}}]});
        let data = json!({"presets":[item.clone(), {"id":"wrong", "label":"Other", "effects":[{"effect":"other/filter", "params":{}}]},
            {"id":"chain", "label":"Chain", "effects":[{"effect":"example/filter", "params":{}}, {"effect":"other/filter", "params":{}}]}]});
        let presets = decode_presets(&data, "example/filter");
        assert_eq!(presets.len(), 1);
        assert_eq!(presets.first().map(|p| &p.params), Some(&json!({"size":6})));
        assert!(decode_presets(&json!({"presets":[{"effects":[]}]}), "example/filter").is_empty());
    }
}
