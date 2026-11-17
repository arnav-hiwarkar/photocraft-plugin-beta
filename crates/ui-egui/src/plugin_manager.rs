#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::cell::RefCell;

    use craft::ui::runtime::env::Label;
    use craft::ui::runtime::memory::SaveKey;
    use craft::ui::shell::ViewportWindow;
    use craft_ext_manager::{Action, Config, Tab};
    use craft_ext_ui_fw::{ManagerState, PluginManager, ProjectPrompt, PromptState};

    use crate::PhotocraftApp;

    thread_local! {
        static WINDOW: RefCell<Option<ManagerState>> = const { RefCell::new(None) };
        static PROMPT: RefCell<Option<PromptState>> = const { RefCell::new(None) };
        static CRASH_SHOWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static UNCLEAN_GIVEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    fn config() -> Result<Config, String> {
        let manager = photocraft_plugins::store::manager().map_err(|e| e.to_string())?;
        let app = manager.app.clone().ok_or_else(|| "PhotoCraft has no plugin app id".to_string())?;
        Ok(Config::app(manager.store.root(), app, manager.provided, manager.platform))
    }

    pub(crate) fn open() -> Result<(), String> {
        let config = config()?;
        WINDOW.with(|w| {
            w.borrow_mut().get_or_insert_with(|| {
                UNCLEAN_GIVEN.with(|g| g.set(false));
                ManagerState::new(config)
            });
        });
        Ok(())
    }

    fn crashed(app: &PhotocraftApp) {
        let unclean = app.session.plugins_unclean_exit();
        if unclean.is_empty() || CRASH_SHOWN.with(std::cell::Cell::get) {
            return;
        }
        CRASH_SHOWN.with(|c| c.set(true));
        if let Err(e) = open() {
            eprintln!("plugins: {e}");
            return;
        }
        WINDOW.with(|w| {
            if let Some(state) = w.borrow_mut().as_mut() {
                state.apply(Action::SetTab(Tab::Problems));
            }
        });
    }

    fn project_prompt(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
        if PROMPT.with(|p| p.borrow().is_none())
            && let Some(statuses) = app.session.take_plugin_project_prompt()
        {
            match config() {
                Ok(config) => PROMPT.with(|p| *p.borrow_mut() = Some(PromptState::new(config, statuses))),
                Err(e) => crate::notices::error(app, e),
            }
        }
        PROMPT.with(|p| {
            let mut slot = p.borrow_mut();
            let Some(state) = slot.as_mut() else { return };
            if ProjectPrompt::new(state).app_names(|_| Label::from("PhotoCraft")).show(ui).is_some() {
                *slot = None;
            }
        });
    }

    pub(crate) fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
        crashed(app);
        project_prompt(app, ui);
        if WINDOW.with(|w| w.borrow().is_none()) {
            return;
        }
        let problems = app.session.plugin_load_problems();
        let unclean = app.session.plugins_unclean_exit();
        let mut links = Vec::new();
        let mut pick_package = false;
        let closed = WINDOW.with(|w| {
            let mut slot = w.borrow_mut();
            let Some(state) = slot.as_mut() else { return false };
            if let Some(id) = state.model().config().scope().cloned() {
                if !UNCLEAN_GIVEN.with(|g| g.replace(true)) {
                    state.set_unclean_exit(id.clone(), unclean.clone());
                }
                state.set_load_problems(id, problems.clone());
            }
            let window = ViewportWindow::new(Label::from(tl!("Plugins").to_string())).remember(SaveKey::named("photocraft.plugins.manager"));
            let out = window.show(ui, |ui, _| {
                let shown = PluginManager::new(state)
                    .app_names(|_| Label::from("PhotoCraft"))
                    .install_file()
                    .show(ui);
                links.extend(shown.links);
                pick_package |= shown.install_file;
            });
            let closed = out.close_requested.is_some();
            if closed {
                *slot = None;
            }
            closed
        });
        let ctx = ui.ctx().clone();
        for url in links {
            crate::links::open(app, &ctx, &url);
        }
        if closed {
            return;
        }
        if pick_package
            && let Err(e) = app.pick_path_filtered(&["craftplugin"], |_, path| {
                WINDOW.with(|w| {
                    if let Some(state) = w.borrow_mut().as_mut() {
                        state.apply(Action::InstallFile { path: path.into(), app: None });
                    }
                });
                Ok(serde_json::Value::Null)
            })
        {
            crate::notices::error(app, e);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::{open, show};

#[cfg(target_arch = "wasm32")]
pub(crate) fn open() -> Result<(), String> {
    Err("Plugin management is available in the desktop app".into())
}
