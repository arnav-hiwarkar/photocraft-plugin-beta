//! PhotoCraft's boundary to the shared Craft plugin platform.
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
    use super::*;
    use craft_ext_store::{
        Manager, Store,
        view::{EnabledRelease, GrantView},
    };
    pub fn manager() -> Result<Manager, Box<dyn std::error::Error>> {
        let c = contract()?;
        Ok(Manager::new(
            Store::at(craft_ext_store::location::default_root()?),
            Some(c.app),
            c.provided,
            format!("{}-{}", if cfg!(target_os = "macos") { "macos" } else { std::env::consts::OS }, std::env::consts::ARCH),
        ))
    }
    /// Start one background refresher per process. Publishing the catalogue advances the
    /// store generation, invalidating registries and session workers on their next pump.
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
    /// The store's generation: it changes with every install, enablement or grant change.
    pub fn generation() -> Result<u64, Box<dyn std::error::Error>> {
        Ok(Store::at(craft_ext_store::location::default_root()?).state()?.generation)
    }
    /// Enabled effects with the display name of the plugin that provides each.
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
    /// Management remains available in safe mode; executable contributions do not.
    pub fn enabled() -> Result<Vec<EnabledRelease>, Box<dyn std::error::Error>> {
        if safe_mode() {
            return Ok(Vec::new());
        }
        let app = AppId::new("photocraft")?;
        let view = GrantView::open(craft_ext_store::location::default_root()?, app.clone())?;
        Ok(view.enabled_contributions(&app)?)
    }
}

/// Run on the caller's worker thread, with a private runtime instance. No document is
/// touched until this returns successfully; the caller owns the undo transaction.
pub fn transform(
    module: &[u8],
    id: &str,
    pixels: &[f32],
    width: u32,
    height: u32,
    color: &str,
    params: Value,
    check: &dyn Fn() -> Result<(), craft_ext_host::wasm::PluginError>,
) -> Result<Vec<f32>, craft_ext_host::wasm::PluginError> {
    use craft_ext_host::wasm::{Filter, Plugin, PluginError};
    if safe_mode() {
        return Err(PluginError::Stopped("Safe mode is enabled".into()));
    }
    check()?;
    let plugin = Plugin::load(id, module)?;
    let mut filter = Filter::new(&plugin, std::sync::Arc::new(|_, _, _| {}))?;
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
    /// Capture and validate the invocation's permission epoch, including the kill list.
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
    /// Refresh enabled contributions before every run and every host fuel slice; `cancelled`
    /// is asked at the same points so a cancelled job stops the guest.
    pub fn run(
        &self,
        pixels: &[f32],
        width: u32,
        height: u32,
        color: &str,
        params: Value,
        generation: u64,
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
        Ok(transform(&bytes, self.plugin.as_str(), pixels, width, height, color, params, &validate)?)
    }
}

pub mod document_thread;
pub mod filter;
pub mod resources;
pub mod session_filters;

#[cfg(not(target_arch = "wasm32"))]
pub mod runtime;
