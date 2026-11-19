//! The watchdog on Linux and macOS (owner's decision of 2026-11-05 to build
//! both now, before their on-device tests): the local socket server
//! ([`server`]) and the hosting of the watchdog around it ([`host`]),
//! shared by the two platforms' entry points (`crate::linux`,
//! `crate::macos`), which add only their folders, their identity rules and
//! their adapter. The Windows service (`crate::windows`) is separate and
//! unchanged.
//!
//! The server is written over two small traits so that it is tested on
//! every OS; the hosting needs Unix sockets and signals, so it only builds
//! there.

#[cfg(unix)]
pub mod host;
pub mod server;

pub use server::{
    ACCEPT_POLL, Conn, Listen, Server, ServerError, ServerHandle, ServerLimits, ServerStats,
};
