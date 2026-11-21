use std::collections::BTreeMap;

use serde_json::Value;

use crate::commands::CommandSpec;

type Row = (&'static str, &'static str, &'static str, &'static str, &'static str);

const FULL_RESOLUTION_PREVIEW: &[&str] = &[
    "filter.distort.ripple",
    "filter.stylize.diffuse",
    "filter.stylize.oilPaint",
    "filter.render.flame",
    "filter.render.fibers",
    "filter.pixelate.mezzotint",
    "filter.noise.addNoise",
    "filter.noise.dustAndScratches",
    "filter.noise.reduceNoise",
    "filter.other.custom",
    "filter.video.deInterlace",
    "filter.stylize.traceContour",
    "filter.stylize.wind",
    "filter.stylize.extrude",
    "filter.render.tree",
    "filter.sharpen.sharpen",
    "filter.sharpen.sharpenMore",
    "filter.sharpen.sharpenEdges",
    "filter.stylize.findEdges",
    "filter.stylize.emboss",
    "filter.noise.despeckle",
    "edit.contentAwareFill",
];

const ANGLE_DIAL: &[&str] = &[
    "filter.blur.motionBlur",
    "filter.stylize.emboss",
    "filter.stylize.oilPaint",
    "filter.render.lightingEffects",
    "filter.render.relight",
    "layer.layerStyle.globalLight",
];

const USES_COLOURS: &[&str] =
    &["filter.render.clouds", "filter.render.differenceClouds", "filter.render.fibers", "filter.stylize.tiles", "filter.pixelate.pointillize"];

fn row(c: &CommandSpec) -> Row {
    let id = c.id;
    (
        c.id,
        if crate::filters::params_for(id, &Value::Null).is_some() && id.starts_with("filter.") { "filter" } else { "command" },
        if FULL_RESOLUTION_PREVIEW.contains(&id) || id.starts_with("filter.gallery.") { "full-resolution" } else { "proxy" },
        if USES_COLOURS.contains(&id) || crate::gallery_cmds::uses_colours(id) { "colours" } else { "none" },
        if ANGLE_DIAL.contains(&id) { "angle" } else { "" },
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
