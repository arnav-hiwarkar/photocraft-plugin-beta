#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::cell::RefCell;

    use craft::ui::buttons::Button;
    use craft::ui::contract::{Props as _, Show as _, ShowContainer as _};
    use craft::ui::display::{Shade, Text};
    use craft::ui::layout_components::{Row, Stack};
    use craft::ui::runtime::env::Label;
    use craft::ui::runtime::memory::SaveKey;
    use craft::ui::shell::ViewportWindow;
    use craft::ui::style::TypeRole;
    use craft_ext_manager::Config;
    use craft_ext_ui_fw::{ManagerState, PluginManager};

    use crate::PhotocraftApp;

    thread_local! { static WINDOW: RefCell<Option<ManagerState>> = const { RefCell::new(None) }; }

    pub(crate) fn open() -> Result<(), String> {
        let manager = photocraft_plugins::store::manager().map_err(|e| e.to_string())?;
        let app = manager.app.clone().ok_or_else(|| "PhotoCraft has no plugin app id".to_string())?;
        let config = Config::app(manager.store.root(), app, manager.provided, manager.platform);
        WINDOW.with(|w| {
            w.borrow_mut().get_or_insert_with(|| ManagerState::new(config));
        });
        Ok(())
    }

    pub(crate) fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
        if WINDOW.with(|w| w.borrow().is_none()) {
            return;
        }
        let paused = app.session.plugins_paused();
        let problems = app.session.plugin_load_problems();
        let mut resume = Vec::new();
        let mut links = Vec::new();
        let closed = WINDOW.with(|w| {
            let mut slot = w.borrow_mut();
            let Some(state) = slot.as_mut() else { return false };
            let window = ViewportWindow::new(Label::from(tl!("Plugins").to_string())).remember(SaveKey::named("photocraft.plugins.manager"));
            let out = window.show(ui, |ui, _| {
                let shown = PluginManager::new(state)
                    .app_names(|_| Label::from("PhotoCraft"))
                    .problems(|ui| {
                        for p in &problems {
                            Stack::new().id_salt(("load-problem", p.plugin.as_str())).show(ui, |ui, _| {
                                Text::new(Label::from(p.name.clone())).role(TypeRole::BodyStrong).show(ui);
                                Text::new(Label::from(p.message.clone())).shade(Shade::Muted).show(ui);
                            });
                        }
                        if !paused.is_empty() {
                            Text::new(tl!("Turned off").to_string()).role(TypeRole::BodyStrong).show(ui);
                            Text::new(tl!("These plugins failed repeatedly or were running when PhotoCraft closed unexpectedly. They stay off until you resume them.").to_string())
                                .shade(Shade::Muted)
                                .show(ui);
                        }
                        for p in &paused {
                            Row::new().id_salt(("paused", p.plugin.as_str())).show(ui, |ui, _| {
                                Text::new(Label::from(p.name.clone())).show(ui);
                                let button = Button::new(tl!("Resume").to_string()).tooltip(Label::from(tl!("Clear the pause and start the plugin again").to_string()));
                                if button.show(ui).clicked() && !resume.contains(&p.plugin) {
                                    resume.push(p.plugin.clone());
                                }
                                Text::new(Label::from(p.reason.clone())).shade(Shade::Faint).show(ui);
                            });
                        }
                    })
                    .show(ui);
                links.extend(shown.links);
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
        for plugin in resume {
            if let Err(e) = app.session.resume_plugin(&plugin) {
                crate::notices::error(app, e.to_string());
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::{open, show};

#[cfg(target_arch = "wasm32")]
pub(crate) fn open() -> Result<(), String> {
    Err("Plugin management is available in the desktop app".into())
}
