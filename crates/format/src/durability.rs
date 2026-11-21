use std::fs::File;
use std::io;
use std::path::Path;

pub use craft_atomic::{Reason, Receipt};

pub fn settle(path: &Path, receipt: Receipt) -> io::Result<Option<Reason>> {
    match receipt {
        Receipt::Durable => Ok(None),
        Receipt::VisibleUnconfirmed(u) if u.reason == Reason::DirSyncFailed => {
            Err(io::Error::new(u.error.kind(), format!("{} was written, but {u}; save again or to another folder", path.display())))
        }
        Receipt::VisibleUnconfirmed(u) => Ok(Some(u.reason)),
        Receipt::Unchanged(error) | Receipt::Displaced { error, .. } => Err(error),
    }
}

pub fn write(path: &Path, bytes: &[u8]) -> io::Result<Option<Reason>> {
    create_parent(path)?;
    settle(path, craft_atomic::write(path, bytes))
}

pub fn write_with(path: &Path, fill: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<Option<Reason>> {
    create_parent(path)?;
    settle(path, craft_atomic::WriteOptions::default().write_with(path, fill))
}

pub fn unconfirmed_warning(path: &Path, reason: Reason) -> String {
    format!("{} was saved, but the save is not confirmed durable and may not survive a power loss or crash ({})", path.display(), reason.id())
}

fn create_parent(path: &Path) -> io::Result<()> {
    match path.parent().filter(|d| !d.as_os_str().is_empty()) {
        Some(dir) => std::fs::create_dir_all(dir).map_err(|e| io::Error::new(e.kind(), format!("cannot create the folder {}: {e}", dir.display()))),
        None => Ok(()),
    }
}
