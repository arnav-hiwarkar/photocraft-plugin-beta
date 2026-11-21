#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod documents;
mod modules;
mod settings;

use std::fmt;
use std::path::PathBuf;

use craft_testkit::corpus::Corpus;

pub use documents::{CANVAS, LAYER_PIXELS};
pub use modules::{ABSENT_ID, FILL_ID, MANIFEST_ID};
pub use settings::{ACTION_NAME, PLUGIN_FOLDER};

pub const APP: &str = "photocraft";

macro_rules! legacy_paths {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum LegacyPath {
            $($variant),+
        }

        impl LegacyPath {
            pub const ALL: [LegacyPath; [$($name),+].len()] = [$(LegacyPath::$variant),+];

            pub fn name(self) -> &'static str {
                match self {
                    $(LegacyPath::$variant => $name),+
                }
            }
        }
    };
}

legacy_paths! {
    Abi1Module => "pc-abi1-module",
    Manifest => "pc-manifest",
    TargetChannel => "apply-target-channel",
    LockTransparency => "apply-lock-transparency",
    SelectionMask => "apply-selection-mask",
    SmartFilterRecord => "smart-filter-record",
    ActionStep => "action-step",
    FolderPreference => "plugin-folder-prefs",
    MissingPlugin => "plugin-missing",
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/legacy-corpus")
}

fn file(path: &str, bytes: impl Into<Vec<u8>>) -> (String, Vec<u8>) {
    (path.to_owned(), bytes.into())
}

pub fn generate() -> Result<Corpus, Error> {
    let fill = modules::fill()?;
    let manifest = modules::manifest()?;
    let mut corpus = Corpus::new(APP)?;
    corpus.add(
        "fill-module",
        LegacyPath::Abi1Module.name(),
        "plugin.run with org.example.corpus.fill sets every sample of the active layer, alpha included, to 64 of 255",
        vec![file("fill.wat", fill.text.clone()), file("fill.wasm", fill.binary.clone())],
    )?;
    corpus.add(
        "manifest-module",
        LegacyPath::Manifest.name(),
        "plugin.list shows a number in px, an int, a bool and a choice with overlap 2 and canvas area; a wrong-typed value is refused",
        vec![file("manifest.wat", manifest.text), file("manifest.wasm", manifest.binary)],
    )?;
    corpus.add(
        "channel-document",
        LegacyPath::TargetChannel.name(),
        "plugin.run fill with target channel 0 sets that alpha channel to 64 and leaves the layer's pixels",
        vec![file("document.pcraft", documents::channel()?)],
    )?;
    corpus.add(
        "locked-document",
        LegacyPath::LockTransparency.name(),
        "plugin.run fill on the transparency-locked layer sets colour to 64 and keeps every alpha",
        vec![file("document.pcraft", documents::locked()?)],
    )?;
    corpus.add(
        "selection-document",
        LegacyPath::SelectionMask.name(),
        "plugin.run fill blends through the saved selection: all of it in column 0, half in column 1, none in columns 2 and 3",
        vec![file("document.pcraft", documents::selection()?)],
    )?;
    corpus.add(
        "smart-filter-document",
        LegacyPath::SmartFilterRecord.name(),
        "the smart object keeps the record {command: plugin.run, params: {id: org.example.corpus.fill, params: {}}} and renders to 64",
        vec![file("document.pcraft", documents::smart_filter(&fill.binary)?)],
    )?;
    corpus.add(
        "action",
        LegacyPath::ActionStep.name(),
        "actions.play Corpus Fill runs the recorded plugin.run step: every sample of the active layer becomes 64",
        vec![file("actions.json", settings::actions()?)],
    )?;
    corpus.add(
        "plugin-folder",
        LegacyPath::FolderPreference.name(),
        "config.toml turns on the Plug-ins folder; loading it installs fill and reports not-a-module.wasm as failed",
        vec![
            file("config.toml", settings::plugin_folder()?),
            file("Plug-ins/fill.wasm", fill.binary),
            file("Plug-ins/not-a-module.wasm", b"not a module\n".to_vec()),
        ],
    )?;
    corpus.add(
        "missing-plugin-document",
        LegacyPath::MissingPlugin.name(),
        "the record for the absent org.example.corpus.absent survives reopening, renders as the unfiltered layer, and plugin.run names it",
        vec![file("document.pcraft", documents::missing_plugin()?)],
    )?;
    Ok(corpus)
}
