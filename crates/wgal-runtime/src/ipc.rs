//! Who may ask the watchdog for what (plan §3.2, §9.2).
//!
//! The platform's IPC server verifies each peer's OS identity and hands it
//! over as a [`Caller`]; it never travels on the wire, and nothing in a
//! request can claim one. [`authorize`] then applies the watchdog's own rules
//! to each decoded request:
//!
//! - a caller that is not `allowed` may make no request at all;
//! - registering a tunnel, and turning on automatic recovery or "start when
//!   stopped externally", widen what the watchdog does with its privileges on
//!   its own, so they need an `elevated` caller or a tunnel on the
//!   installation's [`AccessList`];
//! - everything else needs only `allowed`. That includes turning those
//!   options off, and the wave-three requests: unregistering a tunnel,
//!   pausing or resuming its monitoring, its notification switch, the host
//!   settings (worker threads and time zone) and reading the event log, and
//!   the notification templates of stage 8. They never widen what the
//!   watchdog does with its privileges: they narrow it, restore what was
//!   already allowed, tune how it does what it already does, change only
//!   what a message says, or only read.
//!
//! Tunnels are looked up by the binding the watchdog itself discovered or
//! registered, never by anything the request carries.

use std::collections::BTreeSet;
use std::fmt;

use wgal_core::{Clock, PlatformAdapter, TunnelBinding, Watchdog};
use wgal_protocol::{ErrorCode, ErrorReason, ProtocolError, Request, TunnelId};

/// The OS identity of an IPC peer, as the platform's server verified it.
/// Never travels on the wire.
///
/// `account` is the stable account identifier: a SID string on Windows, a
/// UID elsewhere. `allowed` says the account is on the installation's access
/// list, or is SYSTEM, or an elevated administrator. `elevated` says
/// administrator rights are in effect for this request: SYSTEM or an elevated
/// token. A caller that is `elevated` but not `allowed` is refused like any
/// other caller that is not `allowed`.
///
/// `Debug` leaves the account out, so it cannot reach a log by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct Caller {
    pub account: String,
    pub allowed: bool,
    pub elevated: bool,
}

impl fmt::Debug for Caller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Caller")
            .field("allowed", &self.allowed)
            .field("elevated", &self.elevated)
            .finish_non_exhaustive()
    }
}

/// What the installation's administrator allowed (plan §9.2, "登記
/// allowlist"): the tunnels a caller without administrator rights may
/// register and hand over to automatic actions. Written by the installer into
/// a file only administrators can change; the platform code reads it and
/// hands it over.
#[derive(Clone, Debug, Default)]
pub struct AccessList {
    pub tunnels: BTreeSet<TunnelBinding>,
}

/// The tunnel a request names, if any. Requests that name one wait while a
/// stop or start of that tunnel is out (see `crate::driver`); the others are
/// about the whole host.
pub fn named_tunnel(request: &Request) -> Option<&TunnelId> {
    match request {
        Request::RegisterTunnel { tunnel_id }
        | Request::UnregisterTunnel { tunnel_id }
        | Request::UpdateMonitorSettings { tunnel_id, .. }
        | Request::SetTunnelMonitoring { tunnel_id, .. }
        | Request::ProbeNow { tunnel_id }
        | Request::Connect { tunnel_id }
        | Request::Disconnect { tunnel_id }
        | Request::Reconnect { tunnel_id }
        | Request::SetAutoRecovery { tunnel_id, .. }
        | Request::SetStartWhenStoppedExternally { tunnel_id, .. }
        | Request::SetTunnelNotifications { tunnel_id, .. }
        | Request::SetRecoveryRetry { tunnel_id, .. } => Some(tunnel_id),
        // Reading the event log only filters by tunnel; it changes nothing.
        Request::GetSnapshot
        | Request::DiscoverTunnels
        | Request::UpdateNotificationSettings { .. }
        | Request::SetBotToken { .. }
        | Request::ClearBotToken
        | Request::SetMonitoringPaused { .. }
        | Request::SetMcpEnabled { .. }
        | Request::SetMcpTelegramEnabled { .. }
        | Request::SendTestNotification
        | Request::SetNotificationTemplates { .. }
        | Request::SetCliLanguage { .. }
        | Request::UpdateHostSettings { .. }
        | Request::ReadEventLog { .. }
        // The settings file is about every tunnel; an import judges each.
        | Request::ExportSettings
        | Request::ImportSettings { .. } => None,
    }
}

/// Why a caller that is not `allowed` is refused.
pub(crate) const NOT_ALLOWED: &str = "this account may not use the watchdog on this computer";

fn unauthorized(reason: ErrorReason, message: &str) -> ProtocolError {
    ProtocolError::new(ErrorCode::Unauthorized, Some(reason), message)
}

/// Whether `caller` may make `request` now (see the module documentation).
/// Called on the thread that owns the watchdog, right before the request is
/// handled, so the bindings it checks are the ones the request will act on.
///
/// A tunnel that was not discovered (for registration) or is not registered
/// (for the automatic options) has no binding to find on the list, so a
/// caller without administrator rights is refused even though the watchdog
/// would only have answered `unknown_tunnel`.
pub fn authorize<A: PlatformAdapter, C: Clock>(
    caller: &Caller,
    access: &AccessList,
    watchdog: &Watchdog<A, C>,
    request: &Request,
) -> Result<(), ProtocolError> {
    if !caller.allowed {
        return Err(unauthorized(ErrorReason::CallerNotAllowed, NOT_ALLOWED));
    }
    let listed = |binding: Option<&TunnelBinding>| {
        caller.elevated || binding.is_some_and(|b| access.tunnels.contains(b))
    };
    // No wildcard: a new request must be placed here before it compiles.
    let permitted = match request {
        Request::RegisterTunnel { tunnel_id } => listed(watchdog.discovered_binding(tunnel_id)),
        Request::SetAutoRecovery { tunnel_id, enabled }
        | Request::SetStartWhenStoppedExternally { tunnel_id, enabled } => {
            !*enabled || listed(watchdog.registered_binding(tunnel_id))
        }
        // Unregistering, pausing a tunnel's monitoring and its notification
        // switch only narrow what the watchdog does; resuming monitoring
        // restores what was already allowed, like the host-wide resume. The
        // host settings only tune how it works (worker threads within one
        // per monitored tunnel plus one, the zone times are written in), the
        // notification templates only what a message says, like the
        // notification settings, and reading the event log changes nothing.
        // The recovery retry only tunes how often automatic recovery, which
        // a listed account had to turn on, tries again.
        Request::GetSnapshot
        | Request::DiscoverTunnels
        | Request::UnregisterTunnel { .. }
        | Request::UpdateMonitorSettings { .. }
        | Request::SetTunnelMonitoring { .. }
        | Request::ProbeNow { .. }
        | Request::Connect { .. }
        | Request::Disconnect { .. }
        | Request::Reconnect { .. }
        | Request::SetTunnelNotifications { .. }
        | Request::SetRecoveryRetry { .. }
        | Request::UpdateNotificationSettings { .. }
        | Request::SetBotToken { .. }
        | Request::ClearBotToken
        | Request::SetMonitoringPaused { .. }
        // Like the host-wide pause, they only change who may ask; agents
        // still act as the account that runs them.
        | Request::SetMcpEnabled { .. }
        | Request::SetMcpTelegramEnabled { .. }
        | Request::SendTestNotification
        | Request::SetNotificationTemplates { .. }
        // `wgal`'s shared language only says which language the command
        // line writes in, like the templates (owner decision of 2026-11-19).
        | Request::SetCliLanguage { .. }
        | Request::UpdateHostSettings { .. }
        | Request::ReadEventLog { .. }
        // Exporting only reads; an import authorizes each part it sets as
        // the request that sets it would be.
        | Request::ExportSettings
        | Request::ImportSettings { .. } => true,
    };
    if permitted {
        Ok(())
    } else {
        Err(unauthorized(
            ErrorReason::NeedsAdministratorOrListedTunnel,
            "this tunnel is not on the installation's access list; an administrator must do this",
        ))
    }
}
