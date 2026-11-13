//! Headless PhotoCraft command line. `run` is the whole program, so it can be
//! tested in-process as well as through the binary.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::ffi::OsString;
use std::io::Write;

use clap::CommandFactory;

mod actions;
mod args;
mod automation;
mod batch;
mod document;
mod error;
mod gpu;
mod output;

pub use actions::{ActionsError, Steps, load_actions, load_actions_file, parse_actions};
use args::{Command, parse};
use automation::{commands, mcp, serve};
use batch::{batch, droplet};
use document::{convert, info, run_cmds};
pub(crate) use error::{CliError, Result};

/// The command line's clap definition, for generating shell completions and man pages
/// (`examples/man-and-completions.rs`).
pub fn command() -> clap::Command {
    args::Cli::command()
}

/// Run the CLI on its arguments (without the program name). Returns the process exit code (0 ok,
/// 1 failure, 2 usage). clap reads the arguments as they are: a path that is not valid Unicode (a
/// Latin-1 file name on Linux) is taken, and one where text is expected is a usage error (exit 2).
/// Never a panic, as `std::env::args()` aborts on one (issue #1108).
pub fn run<I, T>(args: I, out: &mut dyn Write, err: &mut dyn Write) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    // Help and `--version` (`help`, `-h`, `<subcommand> --help`, `-V`) come back as errors too,
    // for `out`, exit 0: the subcommand never runs.
    let cli = match parse(&args) {
        Ok(cli) => cli,
        Err(e) => {
            let to: &mut dyn Write = if e.use_stderr() { err } else { out };
            let _ = write!(to, "{}", e.render());
            return e.exit_code();
        }
    };
    if let Command::Plugin { args } = &cli.command {
        if let [verb, id] = args.as_slice() {
            if verb == "resume" {
                return resume(id, out, err);
            }
        }
        if args.as_slice() == ["contract"] {
            return match photocraft_plugins::contract() {
                Ok(c) => if write!(out, "{}", c.to_markdown()).is_ok() { 0 } else { 1 },
                Err(e) => { let _ = writeln!(err, "{e}"); 1 }
            };
        }
        return match photocraft_plugins::store::manager() {
            Ok(m) => craft_ext_cli::run_plugin(&m, args, out, err),
            Err(e) => { let _ = writeln!(err, "{e}"); 1 }
        };
    }
    let result = match &cli.command {
        Command::Plugin { .. } => Ok(()),
        Command::Convert(a) => convert(a, out, err),
        Command::Info(a) => info(a, out),
        Command::Run(a) => run_cmds(a, out, err),
        Command::Batch(a) => batch(a, out, err),
        Command::Droplet(a) => droplet(a, out, err),
        Command::Commands(a) => commands(a, out),
        Command::Mcp(a) => mcp(a, err),
        Command::Serve(a) => serve(a, err),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            1
        }
    }
}

/// `plugin resume <id>`: plugins pause after three failed events in a row, for the life of the
/// process that ran them. A new CLI process starts with nothing paused, so this checks the plugin
/// and says where pauses live: the desktop Plugins window, or `plugin.resume` in a long-lived
/// `serve` / `mcp` session.
fn resume(id: &str, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let mut session = photocraft_engine::Session::new();
    match session.execute("plugin.paused", serde_json::json!({})) {
        Ok(paused) if paused.as_array().is_some_and(|p| p.iter().any(|p| p["plugin"] == id)) => match session.execute("plugin.resume", serde_json::json!({"plugin": id})) {
            Ok(_) => {
                let _ = writeln!(out, "Resumed {id}.");
                0
            }
            Err(e) => {
                let _ = writeln!(err, "error: {e}");
                1
            }
        },
        Ok(_) => {
            let _ = writeln!(
                out,
                "{id} is not paused. Pauses last for one PhotoCraft process: use Resume in the desktop app's Plugins window, or the `plugin.resume` command (params {{\"plugin\":\"{id}\"}}) in a `photocraft-cli serve` or `mcp` session."
            );
            0
        }
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            1
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    /// What [`crate::run`] returned and wrote.
    pub(crate) struct Invoke {
        pub(crate) code: i32,
        pub(crate) stdout: String,
        pub(crate) stderr: String,
    }

    pub(crate) fn invoke(args: &[&str]) -> Invoke {
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let code = crate::run(args, &mut stdout, &mut stderr);
        Invoke { code, stdout: String::from_utf8(stdout).unwrap(), stderr: String::from_utf8(stderr).unwrap() }
    }
}
