use std::fmt;
use std::sync::OnceLock;

use craft_idmap::{ArgsHash, Class, Lock, Migration, Name, Registered, Resolved, Resolver};
use serde_json::Value;

use crate::prefs::TEMPORARY_TOOLS;
use crate::shell_commands::{SHELL_COMMANDS, TOOLS};

pub const LOCK: &str = include_str!("../../../plugin-contract/engine.lock");

pub const MIGRATIONS: &[Migration<Value>] = &[];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Site {
    SmartFilter,
    ActionStep,
    Keymap,
}

impl Site {
    pub const ALL: [Site; 3] = [Site::SmartFilter, Site::ActionStep, Site::Keymap];
}

impl fmt::Display for Site {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Site::SmartFilter => "smart filter",
            Site::ActionStep => "action step",
            Site::Keymap => "keyboard shortcut",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Unresolved {
    #[error("{site}: unknown command `{id}`")]
    Unknown { site: Site, id: String },
    #[error("{site}: command `{id}` was retired")]
    Retired { site: Site, id: String },
    #[error("{site} `{id}`: migration {migration} failed: {reason}")]
    Migration { site: Site, id: String, migration: String, reason: String },
    #[error("{site} `{id}`: engine.lock cannot be read: {reason}")]
    Lock { site: Site, id: String, reason: String },
}

pub struct EngineIds {
    resolver: Resolver,
    migrations: &'static [Migration<Value>],
}

impl EngineIds {
    pub fn new(lock: &str, migrations: &'static [Migration<Value>]) -> Result<EngineIds, String> {
        let lock = Lock::parse(lock).map_err(|e| e.to_string())?;
        let resolver = Resolver::new(lock).map_err(|findings| findings.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))?;
        Ok(EngineIds { resolver, migrations })
    }

    pub fn resolve(&self, site: Site, id: &str, params: Value) -> Result<(&str, Value), Unresolved> {
        match self.resolver.resolve(id, params, self.migrations) {
            Resolved::Current { id, args } => Ok((id.as_str(), args)),
            Resolved::Unknown => Err(Unresolved::Unknown { site, id: id.to_owned() }),
            Resolved::Tombstoned => Err(Unresolved::Retired { site, id: id.to_owned() }),
            Resolved::MigrationFailed { migration, reason, .. } => {
                Err(Unresolved::Migration { site, id: id.to_owned(), migration: migration.to_string(), reason })
            }
        }
    }
}

fn shipped() -> &'static Result<EngineIds, String> {
    static SHIPPED: OnceLock<Result<EngineIds, String>> = OnceLock::new();
    SHIPPED.get_or_init(|| EngineIds::new(LOCK, MIGRATIONS))
}

#[cfg(test)]
thread_local! {
    static SIMULATED: std::cell::Cell<Option<&'static EngineIds>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn simulate<R>(ids: EngineIds, f: impl FnOnce() -> R) -> R {
    let ids: &'static EngineIds = Box::leak(Box::new(ids));
    let before = SIMULATED.with(|s| s.replace(Some(ids)));
    let out = f();
    SIMULATED.with(|s| s.set(before));
    out
}

fn installed() -> Result<&'static EngineIds, &'static str> {
    #[cfg(test)]
    if let Some(ids) = SIMULATED.with(std::cell::Cell::get) {
        return Ok(ids);
    }
    shipped().as_ref().map_err(String::as_str)
}

pub fn resolve_persisted(site: Site, id: &str, params: Value) -> Result<(&'static str, Value), Unresolved> {
    let ids = installed().map_err(|reason| Unresolved::Lock { site, id: id.to_owned(), reason: reason.to_owned() })?;
    ids.resolve(site, id, params)
}

pub fn current_id(site: Site, id: &str) -> Result<&'static str, Unresolved> {
    let ids = installed().map_err(|reason| Unresolved::Lock { site, id: id.to_owned(), reason: reason.to_owned() })?;
    match ids.resolver.current(id) {
        Resolved::Current { id, .. } => Ok(id.as_str()),
        Resolved::Tombstoned => Err(Unresolved::Retired { site, id: id.to_owned() }),
        _ => Err(Unresolved::Unknown { site, id: id.to_owned() }),
    }
}

pub fn registered() -> Result<Vec<Registered>, String> {
    let class = |c: &str| Class::new(c).map_err(|e| e.to_string());
    let (command, query, shell, tool, hold) = (class("command")?, class("query")?, class("shell")?, class("tool")?, class("hold")?);
    let engine = crate::command_specs().iter().map(|c| {
        let args = c.journal.then(|| ArgsHash::of_keys(crate::param_check::named_keys(c.params)));
        (c.id, if c.journal { &command } else { &query }, args)
    });
    let shell = SHELL_COMMANDS.iter().map(|c| (c.0, &shell, None));
    let tools = TOOLS.iter().map(|t| (t.0, &tool, None));
    let holds = TEMPORARY_TOOLS.iter().map(|t| (t.0, &hold, None));
    engine
        .chain(shell)
        .chain(tools)
        .chain(holds)
        .map(|(id, class, args)| Ok(Registered { id: Name::new(id).map_err(|e| format!("{id}: {e}"))?, class: class.clone(), args }))
        .collect()
}

#[cfg(test)]
mod tests;
