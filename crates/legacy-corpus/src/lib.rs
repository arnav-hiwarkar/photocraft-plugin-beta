#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod documents;
mod modules;
mod settings;

use std::fmt;
use std::path::{Path, PathBuf};

use craft_testkit::corpus::Corpus;
use photocraft_engine::prefs::LEGACY_PREFS_FILE;

pub use documents::{CANVAS, GENERATOR, LAYER_ID, LAYER_PIXELS, PROBE_CANVAS, UPPER_LAYER_ID};
pub use modules::{ABSENT_ID, COMMANDS_ID, FILL_ID, MANIFEST_ID, PREVIEW_ID, PROBE_ID};
pub use settings::{ACTION_NAME, CONFIG_FILE, PLUGIN_FOLDER};

pub const APP: &str = "photocraft";

craft_testkit::legacy_paths! {
    pub enum LegacyPath {
        Abi1Module => "pc-abi1-module",
        Manifest => "pc-manifest",
        TargetChannel => "apply-target-channel",
        LockTransparency => "apply-lock-transparency",
        LockPixels => "apply-lock-pixels-refusal",
        SelectionMask => "apply-selection-mask",
        MaskTarget => "apply-mask-target",
        QuickMaskTarget => "apply-quick-mask-target",
        FilterMaskTarget => "apply-filter-mask-target",
        LayerParam => "apply-layer-param",
        SmartFilterApply => "apply-smart-filter",
        SmartFilterRecord => "smart-filter-record",
        ActionStep => "action-step",
        PluginCommands => "plugin-commands",
        PreviewScaling => "preview-scaling",
        FolderPreference => "plugin-folder-prefs",
        LegacyPreferences => "plugin-folder-legacy-json",
        MissingPlugin => "plugin-missing",
    }
}

#[derive(Debug)]
pub enum Error {
    Module(String),
    Document(String),
    Settings(String),
    Corpus(craft_testkit::corpus::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Module(e) => write!(f, "module: {e}"),
            Error::Document(e) => write!(f, "document: {e}"),
            Error::Settings(e) => write!(f, "settings: {e}"),
            Error::Corpus(e) => write!(f, "corpus: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<craft_testkit::corpus::Error> for Error {
    fn from(e: craft_testkit::corpus::Error) -> Self {
        Error::Corpus(e)
    }
}

pub fn root() -> PathBuf {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    crate_dir.ancestors().nth(2).unwrap_or(crate_dir).join("tests/legacy-corpus")
}

fn file(path: &str, bytes: impl Into<Vec<u8>>) -> (String, Vec<u8>) {
    (path.to_owned(), bytes.into())
}

fn document(bytes: Vec<u8>) -> Vec<(String, Vec<u8>)> {
    vec![file("document.pcraft", bytes)]
}

pub fn generate() -> Result<Corpus, Error> {
    let fill = modules::fill()?;
    let manifest = modules::manifest()?;
    let probe = modules::probe()?;
    let commands = modules::commands()?;
    let preview = modules::preview()?;
    let mut corpus = Corpus::new(APP)?;
    let mut add = |name: &str, kind: LegacyPath, expect: &str, files: Vec<(String, Vec<u8>)>| corpus.add(name, kind.name(), expect, files);
    add(
        "fill-module",
        LegacyPath::Abi1Module,
        "plugin.run with org.example.corpus.fill sets every sample of the active layer, alpha included, to 64 of 255",
        vec![file("fill.wat", fill.text.clone()), file("fill.wasm", fill.binary.clone())],
    )?;
    add(
        "probe-module",
        LegacyPath::Abi1Module,
        "the probe writes width 64, height 4, 4 channels, format code 196896 and the exact params JSON plugin.run passes",
        vec![file("probe.wat", probe.text), file("probe.wasm", probe.binary), file("document.pcraft", documents::probe()?)],
    )?;
    add(
        "manifest-module",
        LegacyPath::Manifest,
        "plugin.list shows a number in px, an int, a bool and a choice with overlap 2 and canvas area; a wrong-typed value is refused",
        vec![file("manifest.wat", manifest.text), file("manifest.wasm", manifest.binary)],
    )?;
    add(
        "channel-document",
        LegacyPath::TargetChannel,
        "plugin.run fill with target channel 0 sets that alpha channel to 64 and leaves the layer's pixels",
        document(documents::channel()?),
    )?;
    add(
        "locked-document",
        LegacyPath::LockTransparency,
        "plugin.run fill on the transparency-locked layer sets colour to 64 and keeps every alpha",
        document(documents::locked()?),
    )?;
    add(
        "pixels-locked-document",
        LegacyPath::LockPixels,
        "plugin.run on the layer with locked pixels is refused as locked and leaves the pixels and history alone",
        document(documents::pixels_locked()?),
    )?;
    add(
        "selection-document",
        LegacyPath::SelectionMask,
        "plugin.run fill blends through the saved selection: all of it in column 0, half in column 1, none in columns 2 and 3",
        document(documents::selection()?),
    )?;
    add(
        "mask-document",
        LegacyPath::MaskTarget,
        "plugin.run fill with target mask sets the layer mask to 64 and leaves the pixels",
        document(documents::mask()?),
    )?;
    add(
        "quick-mask-document",
        LegacyPath::QuickMaskTarget,
        "plugin.run fill with target quickMask sets the Quick Mask channel to 64 and leaves the pixels",
        document(documents::quick_mask()?),
    )?;
    add(
        "filter-mask-document",
        LegacyPath::FilterMaskTarget,
        "plugin.run fill with target filterMask sets the smart filter mask to 64 and adds no smart filter",
        document(documents::filter_mask(&fill.binary)?),
    )?;
    add(
        "two-layer-document",
        LegacyPath::LayerParam,
        "plugin.run fill with layer 2 fills the lower layer and leaves the active upper layer",
        document(documents::two_layers()?),
    )?;
    add(
        "smart-apply-document",
        LegacyPath::SmartFilterApply,
        "plugin.run fill on the smart object adds the record and a filter mask taken from the saved selection",
        document(documents::smart_apply()?),
    )?;
    add(
        "smart-filter-document",
        LegacyPath::SmartFilterRecord,
        "the smart object keeps the record {command: plugin.run, params: {id: org.example.corpus.fill, params: {}}} and renders to 64",
        document(documents::smart_filter(&fill.binary)?),
    )?;
    add(
        "action",
        LegacyPath::ActionStep,
        "actions.play Corpus Fill runs the recorded plugin.run step: every sample of the active layer becomes 64",
        vec![file("actions.json", settings::actions()?)],
    )?;
    add(
        "commands-module",
        LegacyPath::PluginCommands,
        "plugin.install by path and by data, a refused second install without replace, plugin.remove and plugin.reload of the folder",
        vec![file("commands.wasm", commands.binary)],
    )?;
    add(
        "preview-module",
        LegacyPath::PreviewScaling,
        "a preview reduced 4 times divides the px parameters radius and size and leaves amount; size's minimum 2 caps the factor",
        vec![file("preview.wat", preview.text), file("preview.wasm", preview.binary)],
    )?;
    add(
        "plugin-folder",
        LegacyPath::FolderPreference,
        "config.toml from the settings store turns on the Plug-ins folder; loading it installs fill and reports not-a-module.wasm as failed",
        vec![
            file(CONFIG_FILE, settings::plugin_folder()?),
            file("Plug-ins/fill.wasm", fill.binary),
            file("Plug-ins/not-a-module.wasm", b"not a module\n".to_vec()),
        ],
    )?;
    add(
        "legacy-preferences",
        LegacyPath::LegacyPreferences,
        "with no config.toml the settings store imports preferences.json and turns on the Plug-ins folder",
        vec![file(LEGACY_PREFS_FILE, settings::legacy_preferences()?)],
    )?;
    add(
        "missing-plugin-document",
        LegacyPath::MissingPlugin,
        "the record for the absent org.example.corpus.absent survives reopening, renders as the unfiltered layer, and plugin.run names it",
        document(documents::missing_plugin()?),
    )?;
    Ok(corpus)
}
