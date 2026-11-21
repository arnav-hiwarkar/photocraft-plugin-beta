use std::collections::BTreeMap;

use serde_json::Value;

use crate::commands::{CommandSpec, ContextInputs, Preview, Role};

type Row = (&'static str, &'static str, &'static str, &'static str, &'static str);

fn row(c: &CommandSpec) -> Row {
    let d = c.declared;
    (
        c.id,
        match d.role {
            Role::Command => "command",
            Role::Filter => "filter",
        },
        match d.preview {
            Preview::Proxy => "proxy",
            Preview::FullResolution => "full-resolution",
        },
        match d.context {
            ContextInputs::None => "none",
            ContextInputs::Colours => "colours",
        },
        d.direction.unwrap_or(""),
    )
}

const PLAIN: (&str, &str, &str, &str) = ("command", "proxy", "none", "");

#[test]
fn every_registered_command_declares_what_the_old_lists_said() {
    let want: BTreeMap<&str, Row> = DECLARED.iter().map(|r| (r.0, *r)).collect();
    let mut wrong = vec![];
    let mut seen = std::collections::BTreeSet::new();
    for c in crate::install::command_specs() {
        let got = row(c);
        seen.insert(c.id);
        let expected = want.get(c.id).copied().unwrap_or((c.id, PLAIN.0, PLAIN.1, PLAIN.2, PLAIN.3));
        if got != expected {
            wrong.push(format!("{}: declared {got:?}, expected {expected:?}", c.id));
        }
    }
    wrong.extend(want.keys().filter(|id| !seen.contains(*id)).map(|id| format!("{id}: not registered")));
    assert!(wrong.is_empty(), "{} commands differ:\n{}", wrong.len(), wrong.join("\n"));
}

#[test]
fn a_command_is_a_filter_exactly_when_it_has_filter_params() {
    let wrong: Vec<String> = crate::install::command_specs()
        .iter()
        .filter(|c| (c.declared.role == Role::Filter) != crate::filters::params_for(c.id, &Value::Null).is_some())
        .map(|c| format!("{}: {:?}", c.id, c.declared.role))
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

fn numeric_range(params: &str, key: &str) -> bool {
    let Some((_, rest)) = params.split_once(&format!("\"{key}\":")) else { return false };
    let Some((lo, _)) = rest.split_once("..") else { return false };
    lo.parse::<f64>().is_ok()
}

#[test]
fn every_declared_direction_is_a_numeric_parameter_of_its_command() {
    let directions: Vec<(&str, &str)> = crate::install::command_specs().iter().filter_map(|c| Some((c.id, c.declared.direction?))).collect();
    assert_eq!(directions.len(), 6);
    let wrong: Vec<&(&str, &str)> = directions.iter().filter(|(id, key)| !crate::install::find(id).is_some_and(|c| numeric_range(c.params, key))).collect();
    assert!(wrong.is_empty(), "{wrong:?}");
    assert!(numeric_range(r#"{"angle":-360..360=0,"distance":1..2000=10}"#, "angle"));
    assert!(!numeric_range(r#"{"angle":"up|down","distance":1..2000=10}"#, "angle"));
    assert!(!numeric_range(r#"{"distance":1..2000=10}"#, "angle"));
}

const DECLARED: &[Row] = &[
    ("edit.contentAwareFill", "command", "full-resolution", "none", ""),
    ("filter.blur.average", "filter", "proxy", "none", ""),
    ("filter.blur.blur", "filter", "proxy", "none", ""),
    ("filter.blur.blurMore", "filter", "proxy", "none", ""),
    ("filter.blur.boxBlur", "filter", "proxy", "none", ""),
    ("filter.blur.gaussianBlur", "filter", "proxy", "none", ""),
    ("filter.blur.lensBlur", "filter", "proxy", "none", ""),
    ("filter.blur.motionBlur", "filter", "proxy", "none", "angle"),
    ("filter.blur.radialBlur", "filter", "proxy", "none", ""),
    ("filter.blur.shapeBlur", "filter", "proxy", "none", ""),
    ("filter.blur.smartBlur", "filter", "proxy", "none", ""),
    ("filter.blur.surfaceBlur", "filter", "proxy", "none", ""),
    ("filter.blurGallery.fieldBlur", "filter", "proxy", "none", ""),
    ("filter.blurGallery.irisBlur", "filter", "proxy", "none", ""),
    ("filter.blurGallery.pathBlur", "filter", "proxy", "none", ""),
    ("filter.blurGallery.spinBlur", "filter", "proxy", "none", ""),
    ("filter.blurGallery.tiltShift", "filter", "proxy", "none", ""),
    ("filter.distort.displace", "filter", "proxy", "none", ""),
    ("filter.distort.pinch", "filter", "proxy", "none", ""),
    ("filter.distort.polarCoordinates", "filter", "proxy", "none", ""),
    ("filter.distort.ripple", "filter", "full-resolution", "none", ""),
    ("filter.distort.shear", "filter", "proxy", "none", ""),
    ("filter.distort.spherize", "filter", "proxy", "none", ""),
    ("filter.distort.twirl", "filter", "proxy", "none", ""),
    ("filter.distort.wave", "filter", "proxy", "none", ""),
    ("filter.distort.zigZag", "filter", "proxy", "none", ""),
    ("filter.filterGallery", "filter", "proxy", "colours", ""),
    ("filter.gallery.accentedEdges", "filter", "full-resolution", "none", ""),
    ("filter.gallery.angledStrokes", "filter", "full-resolution", "none", ""),
    ("filter.gallery.basRelief", "filter", "full-resolution", "colours", ""),
    ("filter.gallery.chalkCharcoal", "filter", "full-resolution", "colours", ""),
    ("filter.gallery.charcoal", "filter", "full-resolution", "colours", ""),
    ("filter.gallery.chrome", "filter", "full-resolution", "none", ""),
    ("filter.gallery.coloredPencil", "filter", "full-resolution", "colours", ""),
    ("filter.gallery.conteCrayon", "filter", "full-resolution", "colours", ""),
    ("filter.gallery.craquelure", "filter", "full-resolution", "none", ""),
    ("filter.gallery.crosshatch", "filter", "full-resolution", "none", ""),
    ("filter.gallery.cutout", "filter", "full-resolution", "none", ""),
    ("filter.gallery.darkStrokes", "filter", "full-resolution", "none", ""),
    ("filter.gallery.diffuseGlow", "filter", "full-resolution", "colours", ""),
    ("filter.gallery.dryBrush", "filter", "full-resolution", "none", ""),
    ("filter.gallery.filmGrain", "filter", "full-resolution", "none", ""),
    ("filter.gallery.fresco", "filter", "full-resolution", "none", ""),
    ("filter.gallery.glass", "filter", "full-resolution", "none", ""),
    ("filter.gallery.glowingEdges", "filter", "full-resolution", "none", ""),
    ("filter.gallery.grain", "filter", "full-resolution", "colours", ""),
    ("filter.gallery.graphicPen", "filter", "full-resolution", "colours", ""),
    ("filter.gallery.halftonePattern", "filter", "full-resolution", "colours", ""),
    ("filter.gallery.inkOutlines", "filter", "full-resolution", "none", ""),
    ("filter.gallery.mosaicTiles", "filter", "full-resolution", "none", ""),
    ("filter.gallery.neonGlow", "filter", "full-resolution", "colours", ""),
    ("filter.gallery.notePaper", "filter", "full-resolution", "colours", ""),
    ("filter.gallery.oceanRipple", "filter", "full-resolution", "none", ""),
    ("filter.gallery.paintDaubs", "filter", "full-resolution", "none", ""),
    ("filter.gallery.paletteKnife", "filter", "full-resolution", "none", ""),
    ("filter.gallery.patchwork", "filter", "full-resolution", "none", ""),
    ("filter.gallery.photocopy", "filter", "full-resolution", "colours", ""),
    ("filter.gallery.plaster", "filter", "full-resolution", "colours", ""),
    ("filter.gallery.plasticWrap", "filter", "full-resolution", "none", ""),
    ("filter.gallery.posterEdges", "filter", "full-resolution", "none", ""),
    ("filter.gallery.reticulation", "filter", "full-resolution", "colours", ""),
    ("filter.gallery.roughPastels", "filter", "full-resolution", "none", ""),
    ("filter.gallery.smudgeStick", "filter", "full-resolution", "none", ""),
    ("filter.gallery.spatter", "filter", "full-resolution", "none", ""),
    ("filter.gallery.sponge", "filter", "full-resolution", "none", ""),
    ("filter.gallery.sprayedStrokes", "filter", "full-resolution", "none", ""),
    ("filter.gallery.stainedGlass", "filter", "full-resolution", "colours", ""),
    ("filter.gallery.stamp", "filter", "full-resolution", "colours", ""),
    ("filter.gallery.sumiE", "filter", "full-resolution", "none", ""),
    ("filter.gallery.texturizer", "filter", "full-resolution", "none", ""),
    ("filter.gallery.tornEdges", "filter", "full-resolution", "colours", ""),
    ("filter.gallery.underpainting", "filter", "full-resolution", "none", ""),
    ("filter.gallery.waterPaper", "filter", "full-resolution", "none", ""),
    ("filter.gallery.watercolor", "filter", "full-resolution", "none", ""),
    ("filter.noise.addNoise", "filter", "full-resolution", "none", ""),
    ("filter.noise.despeckle", "filter", "full-resolution", "none", ""),
    ("filter.noise.dustAndScratches", "filter", "full-resolution", "none", ""),
    ("filter.noise.median", "filter", "proxy", "none", ""),
    ("filter.noise.reduceNoise", "filter", "full-resolution", "none", ""),
    ("filter.other.colorToAlpha", "filter", "proxy", "none", ""),
    ("filter.other.custom", "filter", "full-resolution", "none", ""),
    ("filter.other.highPass", "filter", "proxy", "none", ""),
    ("filter.other.hsbHsl", "filter", "proxy", "none", ""),
    ("filter.other.maximum", "filter", "proxy", "none", ""),
    ("filter.other.minimum", "filter", "proxy", "none", ""),
    ("filter.other.offset", "filter", "proxy", "none", ""),
    ("filter.pixelate.colorHalftone", "filter", "proxy", "none", ""),
    ("filter.pixelate.crystallize", "filter", "proxy", "none", ""),
    ("filter.pixelate.facet", "filter", "proxy", "none", ""),
    ("filter.pixelate.fragment", "filter", "proxy", "none", ""),
    ("filter.pixelate.mezzotint", "filter", "full-resolution", "none", ""),
    ("filter.pixelate.mosaic", "filter", "proxy", "none", ""),
    ("filter.pixelate.pointillize", "filter", "proxy", "colours", ""),
    ("filter.render.clouds", "filter", "proxy", "colours", ""),
    ("filter.render.differenceClouds", "filter", "proxy", "colours", ""),
    ("filter.render.fibers", "filter", "full-resolution", "colours", ""),
    ("filter.render.flame", "command", "full-resolution", "none", ""),
    ("filter.render.lensFlare", "filter", "proxy", "none", ""),
    ("filter.render.lightingEffects", "filter", "proxy", "none", "angle"),
    ("filter.render.relight", "filter", "proxy", "none", "angle"),
    ("filter.render.tree", "command", "full-resolution", "none", ""),
    ("filter.sharpen.sharpen", "filter", "full-resolution", "none", ""),
    ("filter.sharpen.sharpenEdges", "filter", "full-resolution", "none", ""),
    ("filter.sharpen.sharpenMore", "filter", "full-resolution", "none", ""),
    ("filter.sharpen.smartSharpen", "filter", "proxy", "none", ""),
    ("filter.sharpen.unsharpMask", "filter", "proxy", "none", ""),
    ("filter.stylize.diffuse", "filter", "full-resolution", "none", ""),
    ("filter.stylize.emboss", "filter", "full-resolution", "none", "angle"),
    ("filter.stylize.extrude", "filter", "full-resolution", "none", ""),
    ("filter.stylize.findEdges", "filter", "full-resolution", "none", ""),
    ("filter.stylize.oilPaint", "filter", "full-resolution", "none", "angle"),
    ("filter.stylize.solarize", "filter", "proxy", "none", ""),
    ("filter.stylize.tiles", "filter", "proxy", "colours", ""),
    ("filter.stylize.traceContour", "filter", "full-resolution", "none", ""),
    ("filter.stylize.wind", "filter", "full-resolution", "none", ""),
    ("filter.video.deInterlace", "filter", "full-resolution", "none", ""),
    ("filter.video.ntscColors", "filter", "proxy", "none", ""),
    ("layer.layerStyle.globalLight", "command", "proxy", "none", "angle"),
];
