//! `wgal`: WGAL's command line (owner decisions of 2026-11-18), in English
//! or in one of the window's 13 languages (owner decisions of 2026-11-19).
//! It does from a terminal what the window does with the watchdog: shows
//! its state, registers tunnels, changes their settings, connects and
//! disconnects them, sets up Telegram, reads the event log, and exports and
//! imports the settings file. It is the user's own tool: every request goes
//! through the watchdog's pipe or socket as the window's does, with no
//! origin, and the watchdog checks the account and the access list as for
//! the window. Whoever runs it as an allowed account, an agent with a shell
//! too, acts as the user (owner decision of 2026-11-18).
//!
//! It runs no command and takes no file path, executable or service name
//! (AGENTS.md rule 8): the settings file comes only from standard input and
//! goes only to standard output. The Bot Token is never taken from the
//! command line or the environment, only typed unseen at the terminal or
//! read from standard input, and nothing it writes holds text shaped like a
//! token.

use std::io;
use std::process::ExitCode;

mod args;
mod console;
mod languages;
mod run;
mod show;
mod text;

#[cfg(test)]
mod tests;

fn main() -> ExitCode {
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let mut watchdog = wgal_client::PipeWatchdog::new(wgal_client::Client::Cli);
    let mut console = console::Terminal::new();
    let report = run::run(&arguments, &mut watchdog, &mut console);
    let code = report.emit(&mut io::stdout().lock(), &mut io::stderr().lock());
    ExitCode::from(code)
}
