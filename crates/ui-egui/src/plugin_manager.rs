//! Shared plugin manager, owned by the UI thread and available in safe mode.
#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::{cell::RefCell, sync::Arc};
    struct Window {
        view: craft_ext_ui::ManagerView,
        manager: Arc<craft_ext_store::Manager>,
    }
    thread_local! { static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) }; }
    pub fn open() -> Result<(), String> {
        let manager = photocraft_plugins::store::manager().map_err(|e| e.to_string())?;
        WINDOW.with(|w| *w.borrow_mut() = Some(Window { view: craft_ext_ui::ManagerView::new(), manager: Arc::new(manager) }));
        Ok(())
    }
    pub fn show(ctx: &egui::Context) {
        WINDOW.with(|w| {
            let mut slot = w.borrow_mut();
            if let Some(window) = slot.as_mut() {
                let mut open = true;
                egui::Window::new("Plugins").open(&mut open).default_width(700.0).show(ctx, |ui| window.view.show(ui, &window.manager));
                if !open {
                    *slot = None;
                }
            }
        });
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub use native::{open, show};
#[cfg(target_arch = "wasm32")]
pub fn open() -> Result<(), String> {
    Err("Plugin management is available in the desktop app".into())
}
#[cfg(target_arch = "wasm32")]
pub fn show(_: &egui::Context) {}
