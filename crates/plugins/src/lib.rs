#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
use craft_ext_core::id::AppId;
use serde_json::Value;

pub mod contract;
pub use contract::contract;

pub fn safe_mode() -> bool {
    std::env::var_os("PHOTOCRAFT_SAFE_MODE").is_some_and(|v| v == "1")
}

#[cfg(not(target_arch = "wasm32"))]
pub mod store {
    use super::{AppId, contract, safe_mode};
    use craft_ext_store::{
        Manager, Store,
        view::{EnabledRelease, GrantView},
    };
    pub fn open() -> Result<Store, Box<dyn std::error::Error>> {
        Ok(Store::at(craft_ext_store::location::default_root()?))
    }
    pub fn app() -> Result<AppId, Box<dyn std::error::Error>> {
        Ok(contract()?.app)
    }
    pub fn manager() -> Result<Manager, Box<dyn std::error::Error>> {
        let c = contract()?;
        Ok(Manager::new(
            open()?,
            Some(c.app),
            c.provided,
            format!("{}-{}", if cfg!(target_os = "macos") { "macos" } else { std::env::consts::OS }, std::env::consts::ARCH),
        ))
    }
    pub fn start_catalogue_refresh() {
        static START: std::sync::Once = std::sync::Once::new();
        START.call_once(|| {
            if let Err(e) = std::thread::Builder::new().name("plugin-catalogue".into()).spawn(|| {
                loop {
                    let refresh = || -> Result<(), Box<dyn std::error::Error>> {
                        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
                        manager()?.refresh(now)?;
                        Ok(())
                    };
                    if let Err(e) = refresh() {
                        eprintln!("Plugin catalogue refresh failed: {e}");
                    }
                    std::thread::sleep(std::time::Duration::from_secs(6 * 60 * 60));
                }
            }) {
                eprintln!("Could not start plugin catalogue refresh: {e}");
            }
        });
    }
    #[derive(Clone)]
    pub struct FilterSource {
        pub plugin: craft_ext_core::id::ExtensionId,
        pub version: craft_ext_core::version::Version,
        pub root: std::path::PathBuf,
        pub contribution: craft_ext_core::manifest::Contribution,
    }
    pub fn filters() -> Result<Vec<FilterSource>, Box<dyn std::error::Error>> {
        Ok(named_filters()?.into_iter().map(|(filter, _)| filter).collect())
    }
    pub fn generation() -> Result<u64, Box<dyn std::error::Error>> {
        Ok(open()?.state()?.app_fingerprint(&app()?))
    }
    /// The project reference for `plugin`'s release active in PhotoCraft, if it is installed.
    pub fn installed_reference(plugin: &craft_ext_core::id::ExtensionId) -> Option<craft_ext_core::project::PluginRef> {
        let store = open().ok()?;
        let state = store.state().ok()?;
        let version = state.apps.get(&app().ok()?)?.get(plugin)?.active?;
        let package = state.packages.get(plugin)?;
        let release = package.releases.get(&version)?;
        let name = store.manifest(&state, plugin, version).ok()?.name;
        let source = match package.origin {
            craft_ext_store::state::Origin::Catalogue { .. } => craft_ext_core::project::Source::Catalogue,
            _ => craft_ext_core::project::Source::Local,
        };
        Some(craft_ext_core::project::PluginRef { id: plugin.clone(), version, sha256: Some(release.sha256.clone()), source, name, contributions: vec![], state_versions: None })
    }
    pub fn named_filters() -> Result<Vec<(FilterSource, String)>, Box<dyn std::error::Error>> {
        let provided = contract()?.provided;
        let mut filters = Vec::new();
        for (plugin, version, root, manifest, enabled) in enabled()? {
            for contribution in manifest.contributions {
                if enabled.contains(&contribution.id)
                    && contribution.kind == craft_ext_core::manifest::ContributionKind::Effect
                    && contribution.contract.check(&provided).is_none()
                {
                    filters.push((FilterSource { plugin: plugin.clone(), version, root: root.clone(), contribution }, manifest.name.clone()));
                }
            }
        }
        Ok(filters)
    }
    pub fn enabled() -> Result<Vec<EnabledRelease>, Box<dyn std::error::Error>> {
        if safe_mode() {
            return Ok(Vec::new());
        }
        let app = app()?;
        let view = GrantView::open(craft_ext_store::location::default_root()?, app.clone())?;
        Ok(view.enabled_contributions(&app)?)
    }
    pub fn quarantined(store: &Store) -> Result<Vec<crate::runtime::Paused>, Box<dyn std::error::Error>> {
        let app = app()?;
        let state = store.state()?;
        let mut out = Vec::new();
        for (id, setup) in state.apps.get(&app).into_iter().flatten() {
            if let (Some(q), Some(active)) = (&setup.quarantine, setup.active)
                && q.version == active
            {
                let name = store.manifest(&state, id, active).map_or_else(|_| id.to_string(), |m| m.name);
                out.push(crate::runtime::Paused { plugin: id.to_string(), name, reason: q.reason.clone() });
            }
        }
        Ok(out)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn transform(
    module: &[u8],
    id: &str,
    pixels: &[f32],
    width: u32,
    height: u32,
    color: &str,
    params: Value,
    progress: craft_ext_host::wasm::ProgressFn,
    check: &dyn Fn() -> Result<(), craft_ext_host::wasm::PluginError>,
) -> Result<Vec<f32>, craft_ext_host::wasm::PluginError> {
    use craft_ext_host::wasm::{Filter, Plugin, PluginError};
    if safe_mode() {
        return Err(PluginError::Stopped("Safe mode is enabled".into()));
    }
    check()?;
    let plugin = Plugin::load(id, module)?;
    let mut filter = Filter::new(&plugin, std::sync::Arc::new(|_, _, _| {}))?;
    filter.set_progress(progress);
    let mut output = pixels.to_vec();
    let ctx = serde_json::json!({"params":params,"time":0.0,"scale":1.0,"origin":[0,0],"full":[width,height],"quality":"final","seed":0,"color":color});
    filter.run(&ctx, &mut output, width, height, check)?;
    check()?;
    Ok(output)
}

#[cfg(not(target_arch = "wasm32"))]
impl store::FilterSource {
    fn form_plugin(&self) -> Result<craft_ext_host::wasm::Plugin, Box<dyn std::error::Error>> {
        use std::io::Read as _;
        self.validate(self.grant_generation()?)?;
        let module = self.contribution.module.as_ref().ok_or("Filter has no module")?;
        let mut bytes = Vec::new();
        std::fs::File::open(self.root.join(module))?.take(craft_ext_host::wasm::MAX_MODULE_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        Ok(craft_ext_host::wasm::Plugin::load(self.plugin.as_str(), &bytes)?)
    }
    pub fn form(&self) -> Result<craft_ext_core::form::Form, Box<dyn std::error::Error>> {
        let plugin = self.form_plugin()?;
        let epoch = self.grant_generation()?;
        let start = std::time::Instant::now();
        let check = || {
            if start.elapsed() > std::time::Duration::from_millis(100) {
                return Err(craft_ext_host::wasm::PluginError::Stopped("Form deadline exceeded".into()));
            }
            self.validate(epoch).map_err(|e| craft_ext_host::wasm::PluginError::Stopped(e.to_string()))
        };
        let form = plugin
            .describe(std::sync::Arc::new(|_, _, _| {}), &check)?
            .or_else(|| self.contribution.form.clone())
            .unwrap_or_else(|| filter::empty_form(&self.contribution.id, &self.contribution.label));
        if form.params.iter().any(|p| p.key.starts_with("__") || filter::RESERVED_PARAMS.contains(&p.key.as_str())) {
            return Err("The form uses a reserved command key".into());
        }
        Ok(form)
    }
    pub fn update(&self, values: &craft_ext_core::form::Values, rev: u64) -> Result<Option<craft_ext_core::form::Patch>, Box<dyn std::error::Error>> {
        let plugin = self.form_plugin()?;
        let epoch = self.grant_generation()?;
        let start = std::time::Instant::now();
        Ok(plugin.update(values, rev, std::sync::Arc::new(|_, _, _| {}), &|| {
            if start.elapsed() > std::time::Duration::from_millis(100) {
                return Err(craft_ext_host::wasm::PluginError::Stopped("Form deadline exceeded".into()));
            }
            self.validate(epoch).map_err(|e| craft_ext_host::wasm::PluginError::Stopped(e.to_string()))
        })?)
    }
    pub fn grant_generation(&self) -> Result<u64, Box<dyn std::error::Error>> {
        let view = craft_ext_store::view::GrantView::open(craft_ext_store::location::default_root()?, contract()?.app)?;
        Ok(view.generation(&self.plugin))
    }
    pub fn validate(&self, generation: u64) -> Result<(), Box<dyn std::error::Error>> {
        if self.grant_generation()? != generation
            || !store::enabled()?.iter().any(|(id, version, root, _, enabled)| {
                id == &self.plugin && version == &self.version && root == &self.root && enabled.contains(&self.contribution.id)
            })
        {
            return Err("The plugin is disabled, revoked or its permissions changed".into());
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &self,
        pixels: &[f32],
        width: u32,
        height: u32,
        color: &str,
        params: Value,
        generation: u64,
        progress: craft_ext_host::wasm::ProgressFn,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
        use craft_ext_host::wasm::PluginError;
        let validate = || -> Result<(), PluginError> {
            if cancelled() {
                return Err(PluginError::Stopped("Cancelled".into()));
            }
            self.validate(generation).map_err(|e| PluginError::Stopped(e.to_string()))
        };
        validate()?;
        let module = self.contribution.module.as_ref().ok_or("The filter has no Wasm module")?;
        use std::io::Read as _;
        let mut bytes = Vec::new();
        std::fs::File::open(self.root.join(module))?.take(craft_ext_host::wasm::MAX_MODULE_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        Ok(transform(&bytes, self.plugin.as_str(), pixels, width, height, color, params, progress, &validate)?)
    }
}

/// Records that the document uses `plugin` (and its `contribution`, for a resource such as a
/// LUT) in `refs`. `installed` is its active release here: the reference moves to it; without one
/// the recorded reference is kept, so a missing plugin is never dropped or invented.
pub fn record_use(refs: &mut Vec<craft_ext_core::project::PluginRef>, plugin: &craft_ext_core::id::ExtensionId, contribution: Option<&str>, installed: Option<craft_ext_core::project::PluginRef>) {
    let existing = refs.iter().position(|r| &r.id == plugin);
    let mut contributions = existing.map(|i| refs[i].contributions.clone()).unwrap_or_default();
    if let Some(c) = contribution.filter(|c| !contributions.iter().any(|x| x == c)) {
        contributions.push(c.to_string());
        contributions.sort();
    }
    match (existing, installed) {
        (Some(i), Some(current)) => refs[i] = craft_ext_core::project::PluginRef { contributions, ..current },
        (None, Some(current)) => refs.push(craft_ext_core::project::PluginRef { contributions, ..current }),
        (Some(i), None) => refs[i].contributions = contributions,
        (None, None) => {}
    }
}

pub mod document_thread;
pub mod filter;
pub mod resources;
pub mod session_filters;

#[cfg(not(target_arch = "wasm32"))]
pub mod runtime;

#[cfg(test)]
mod reference_tests {
    use super::record_use;
    use craft_ext_core::{id::ExtensionId, project::{PluginRef, Source}, version::Version};

    fn reference(version: &str, contributions: &[&str]) -> PluginRef {
        PluginRef {
            id: ExtensionId::new("io.github.alice.looks").unwrap(),
            version: Version::parse(version).unwrap(),
            sha256: Some("aa".repeat(32)),
            source: Source::Catalogue,
            name: "Looks".into(),
            contributions: contributions.iter().map(|c| c.to_string()).collect(),
            state_versions: None,
        }
    }

    #[test]
    fn uses_are_recorded_against_the_installed_release_and_kept_while_missing() {
        let id = ExtensionId::new("io.github.alice.looks").unwrap();
        let mut refs = vec![];
        record_use(&mut refs, &id, None, None);
        assert_eq!(refs, vec![], "nothing is invented for a plugin that is not installed");
        record_use(&mut refs, &id, None, Some(reference("1.0.0", &[])));
        record_use(&mut refs, &id, Some("film"), Some(reference("1.1.0", &[])));
        assert_eq!(refs, vec![reference("1.1.0", &["film"])]);
        record_use(&mut refs, &id, Some("cool"), None);
        assert_eq!(refs, vec![reference("1.1.0", &["cool", "film"])], "a missing plugin keeps its recorded release");
    }
}
