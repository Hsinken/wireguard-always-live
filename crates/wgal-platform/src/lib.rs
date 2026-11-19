//! Platform adapters: one [`wgal_core::PlatformAdapter`] per official WireGuard
//! manager. Windows drives `WireGuardTunnel$<name>` services through the
//! Service Control Manager, macOS drives the official app's VPN configurations
//! through `/usr/sbin/scutil --nc`, and Ubuntu drives `wg-quick@<if>.service`
//! units through systemd.
//!
//! Each adapter module compiles on every OS, so its parsers and its use of a
//! [`exec::Runner`] are unit-tested everywhere with anonymized fixtures and
//! [`exec::ScriptedRunner`]. Only code that calls an OS API directly sits
//! behind `#[cfg(target_os = ...)]`. Code added here must:
//!
//! - run tools only through [`exec`]: a fixed absolute executable with a fixed
//!   argument vector, never a shell, after validating every argument (UUID,
//!   unit, service name, interface, IPv4 address, port);
//! - parse output with strict, bounded parsers that fail on missing fields,
//!   duplicates, truncated output or unknown formats;
//! - act only on the tunnel it was given, and never edit, export or recreate
//!   WireGuard configurations, keys or services;
//! - keep `unsafe` inside the one module per platform allowed to call OS APIs,
//!   with a `// SAFETY:` comment on every block (see AGENTS.md).

pub mod exec;
pub mod linux;
pub mod macos;
pub mod socket;
pub mod terminal;
pub mod windows;
