use std::fs::File;
use std::io;
use std::path::Path;

pub use craft_atomic::{Reason, Reasons, Receipt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    Durable,
    Unconfirmed(Reasons),
    NotMeasured,
}

impl Durability {
    pub fn of(unconfirmed: Option<Reasons>) -> Self {
        unconfirmed.map_or(Self::Durable, Self::Unconfirmed)
    }

    pub fn id(self) -> String {
        match self {
            Self::Durable => "durable".into(),
            Self::NotMeasured => "not-measured".into(),
            Self::Unconfirmed(reasons) => reasons.ids().join("+"),
        }
    }

    pub fn unconfirmed(self) -> Option<Reasons> {
        match self {
            Self::Unconfirmed(reasons) => Some(reasons),
            Self::Durable | Self::NotMeasured => None,
        }
    }
}

pub fn settle(path: &Path, receipt: Receipt) -> io::Result<Option<Reasons>> {
    match receipt {
        Receipt::VisibleUnconfirmed(u) if u.reasons().contains(Reason::DirSyncFailed) => {
            let kind = u.causes().find(|(r, _)| *r == Reason::DirSyncFailed).map_or(io::ErrorKind::Other, |(_, e)| e.kind());
            Err(io::Error::new(kind, format!("{} was written, but {u}; save again or to another folder", path.display())))
        }
        other => other.published(),
    }
}

pub fn write(path: &Path, bytes: &[u8]) -> io::Result<Option<Reasons>> {
    create_parent(path)?;
    settle(path, craft_atomic::write(path, bytes))
}

pub fn write_with(path: &Path, fill: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<Option<Reasons>> {
    create_parent(path)?;
    settle(path, craft_atomic::WriteOptions::default().write_with(path, fill))
}

pub fn unconfirmed_warning(path: &Path, reasons: Reasons) -> String {
    format!("{} was saved, but the save is not confirmed durable and may not survive a power loss or crash ({reasons})", path.display())
}

fn create_parent(path: &Path) -> io::Result<()> {
    match path.parent().filter(|d| !d.as_os_str().is_empty()) {
        Some(dir) => std::fs::create_dir_all(dir).map_err(|e| io::Error::new(e.kind(), format!("cannot create the folder {}: {e}", dir.display()))),
        None => Ok(()),
    }
}
