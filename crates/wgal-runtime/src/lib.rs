//! The watchdog process: the single source of tunnel state. It serves the
//! local IPC endpoint (an ACL-restricted named pipe on Windows, a Unix domain
//! socket in a protected directory elsewhere; never a network listener),
//! persists intent, incidents and backoff, sends notifications, and provides
//! the entry point the OS service manager starts.
//!
//! Code here must verify each IPC peer's OS identity, decode and encode only
//! through `wgal_protocol::codec`, keep a single writer per host or user, and
//! keep secrets out of logs, diagnostics and error text.

#[cfg(any(target_os = "linux", test))]
pub mod linux;
#[cfg(any(target_os = "macos", test))]
pub mod macos;
pub mod notify;
#[cfg(any(unix, test))]
pub mod socket;
pub mod store;
#[cfg(windows)]
pub mod windows;

pub use store::{FileStore, Source, Writer};

pub mod driver;
pub mod events;
pub mod ipc;

pub use driver::{Client, Running, spawn};
pub use events::EventLog;
pub use ipc::{AccessList, Caller};
