#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::cell::RefCell;
    use std::sync::Arc;

    use craft::ui::runtime::env::Label;
    use craft::ui::runtime::memory::SaveKey;
    use craft::ui::shell::ViewportWindow;

    use crate::PhotocraftApp;

    struct Window {
        view: craft_ext_ui::ManagerView,
        manager: Arc<craft_ext_store::Manager>,
    }

    thread_local! { static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) }; }

    pub(crate) fn open() -> Result<(), String> {
        let manager = photocraft_plugins::store::manager().map_err(|e| e.to_string())?;
        WINDOW.with(|w| {
            let mut slot = w.borrow_mut();
            if slot.is_none() {
                *slot = Some(Window { view: craft_ext_ui::ManagerView::new(), manager: Arc::new(manager) });
            }
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
        let closed = WINDOW.with(|w| {
            let mut slot = w.borrow_mut();
            let Some(window) = slot.as_mut() else { return false };
            let out = ViewportWindow::new(Label::from(tl!("Plugins").to_string())).remember(SaveKey::named("photocraft.plugins.manager")).show(ui, |ui, _| {
                window.view.show_with_app_problems(ui, &window.manager, |ui| {
                    for p in &problems {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(&p.name).on_hover_text(&p.plugin);
                            ui.weak(&p.message);
                        });
                    }
                    if !paused.is_empty() {
                        ui.strong(tl!("Turned off"));
                        ui.weak(tl!("These plugins failed repeatedly or were running when PhotoCraft closed unexpectedly. They stay off until you resume them."));
                    }
                    for p in &paused {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(&p.name).on_hover_text(&p.plugin);
                            if ui.button(tl!("Resume")).on_hover_text(tl!("Clear the pause and start the plugin again")).clicked() && !resume.contains(&p.plugin) {
                                resume.push(p.plugin.clone());
                            }
                            ui.weak(&p.reason);
                        });
                    }
                });
            });
            let closed = out.close_requested.is_some();
            if closed {
                *slot = None;
            }
            closed
        });
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
