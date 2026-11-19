//! The Windows service (WGW-001-D): the SCM entry point ([`service`]), the
//! hosting of the watchdog in the installation's folder ([`host`]), the
//! ACL-restricted named pipe the GUI talks to ([`server`]), the
//! installation's access list ([`access`]), the service's own log
//! ([`log`]) and the network change notices that restart the grace period
//! (`network`). Every Win32 call they make outside the `windows-service`
//! crate goes through the safe wrappers in `wgal_platform::windows` (its
//! `sys.rs` is the only file allowed `unsafe`).

pub mod access;
pub mod host;
pub mod log;
mod network;
pub mod server;
pub mod service;

pub use access::{AccessError, AccessList, LineProblem, UserSid};
pub use server::{PeerIdentity, Server, ServerError, ServerHandle, ServerLimits, ServerStats};
pub use service::{SERVICE_NAME, main};
