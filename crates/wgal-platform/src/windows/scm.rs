//! The Service Control Manager boundary: what the adapter asks of the SCM, and
//! how the SCM's answers and errors map to adapter results.

use std::fmt;

use wgal_core::AdapterError;
use wgal_protocol::{AdapterErrorKind, OsState};

use super::name::ServiceName;

/// Most services one enumeration returns. A host with more is not a host
/// this adapter understands; the enumeration fails instead of truncating.
pub const MAX_SERVICES: usize = 4096;

/// The only two requests that change a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Start,
    Stop,
}

/// The Service Control Manager, reduced to what the adapter needs. Services
/// are named only by [`ServiceName`], which only a validated tunnel name can
/// produce, so nothing but `WireGuardTunnel$<name>` services can be opened.
/// Implementations open each service with the least access the call needs
/// and never create, delete or reconfigure a service.
///
/// A start or stop is split into [`open`](Self::open), which may block while
/// the SCM is busy but changes nothing, and [`send`](Self::send), the one
/// call that changes the service. The adapter decides between the two
/// whether the request may still be sent (see `Lane` in the adapter).
pub trait ServiceControl: Send + Sync + fmt::Debug {
    /// An open service handle with the access one [`Control`] needs.
    type Handle;

    /// Names of every Win32 service, without opening any of them.
    fn service_names(&self) -> Result<Vec<String>, ScmError>;

    /// Opens the service with query access only.
    fn query(&self, service: &ServiceName) -> Result<ServiceState, ScmError>;

    /// Opens the service with start access only (for [`Control::Start`]) or
    /// stop access only (for [`Control::Stop`]). Sends nothing.
    fn open(&self, service: &ServiceName, control: Control) -> Result<Self::Handle, ScmError>;

    /// Sends `control` through a handle from [`open`](Self::open): asks the
    /// SCM to start the service, or sends it the stop control. It can block
    /// until the service reacts, and once called it cannot be withdrawn.
    fn send(&self, handle: &Self::Handle, control: Control) -> Result<(), ScmError>;
}

/// `dwCurrentState` of a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceState {
    Stopped,
    StartPending,
    StopPending,
    Running,
    ContinuePending,
    PausePending,
    Paused,
    /// A value the documented set does not contain.
    Other(u32),
}

impl ServiceState {
    pub fn from_raw(value: u32) -> Self {
        match value {
            1 => Self::Stopped,
            2 => Self::StartPending,
            3 => Self::StopPending,
            4 => Self::Running,
            5 => Self::ContinuePending,
            6 => Self::PausePending,
            7 => Self::Paused,
            other => Self::Other(other),
        }
    }

    /// Whether the service is between states. While it is, the SCM refuses a
    /// stop control with `SERVICE_CANNOT_ACCEPT_CTRL`.
    pub fn is_pending(self) -> bool {
        matches!(
            self,
            Self::StartPending | Self::StopPending | Self::ContinuePending | Self::PausePending
        )
    }

    /// What the manager reports. A running service only means the tunnel is
    /// up as far as the manager knows, never that traffic flows (plan §4.2).
    /// Tunnel services do not pause, so a paused one is `Unknown`, and an
    /// undocumented state is a parse error rather than a guess.
    pub fn os_state(self) -> Result<OsState, AdapterError> {
        match self {
            Self::Stopped => Ok(OsState::Disconnected),
            Self::Running => Ok(OsState::Connected),
            Self::StartPending | Self::StopPending | Self::ContinuePending | Self::PausePending => {
                Ok(OsState::Transitioning)
            }
            Self::Paused => Ok(OsState::Unknown),
            Self::Other(value) => Err(AdapterError::new(
                AdapterErrorKind::ParseError,
                &format!("the tunnel service reported undocumented state {value}"),
            )),
        }
    }
}

/// A failed SCM call, by its Win32 error code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScmError {
    pub code: u32,
}

impl ScmError {
    pub const ACCESS_DENIED: Self = Self { code: 5 };
    pub const INVALID_NAME: Self = Self { code: 123 };
    pub const SERVICE_REQUEST_TIMEOUT: Self = Self { code: 1053 };
    pub const SERVICE_DATABASE_LOCKED: Self = Self { code: 1055 };
    pub const SERVICE_ALREADY_RUNNING: Self = Self { code: 1056 };
    pub const SERVICE_DISABLED: Self = Self { code: 1058 };
    pub const SERVICE_DOES_NOT_EXIST: Self = Self { code: 1060 };
    pub const SERVICE_CANNOT_ACCEPT_CTRL: Self = Self { code: 1061 };
    pub const SERVICE_NOT_ACTIVE: Self = Self { code: 1062 };
    pub const SERVICE_DEPENDENCY_FAIL: Self = Self { code: 1068 };
    pub const SERVICE_MARKED_FOR_DELETE: Self = Self { code: 1072 };
    pub const SHUTDOWN_IN_PROGRESS: Self = Self { code: 1115 };

    /// The adapter error for a failed call. A service that no longer exists
    /// (or is being deleted) is `NotFound`: the binding is lost and WGAL
    /// never recreates it (plan §4.2). It is never read as `Disconnected`.
    pub fn to_adapter_error(self) -> AdapterError {
        let (kind, detail) = match self {
            Self::SERVICE_DOES_NOT_EXIST | Self::SERVICE_MARKED_FOR_DELETE => (
                AdapterErrorKind::NotFound,
                "the tunnel service no longer exists; register the tunnel again".to_owned(),
            ),
            Self::ACCESS_DENIED => (
                AdapterErrorKind::PermissionDenied,
                "the Service Control Manager denied access to the tunnel service".to_owned(),
            ),
            Self::SERVICE_REQUEST_TIMEOUT => (
                AdapterErrorKind::Timeout,
                "the tunnel service did not answer the Service Control Manager in time".to_owned(),
            ),
            Self::SERVICE_DISABLED => (
                AdapterErrorKind::ManagerConflict,
                "the tunnel service is disabled; WGAL does not change its start type".to_owned(),
            ),
            Self::SERVICE_DATABASE_LOCKED | Self::SERVICE_CANNOT_ACCEPT_CTRL => (
                AdapterErrorKind::ManagerConflict,
                "the tunnel service cannot take this request right now".to_owned(),
            ),
            Self::SHUTDOWN_IN_PROGRESS => (
                AdapterErrorKind::ManagerConflict,
                "Windows is shutting down".to_owned(),
            ),
            Self { code } => (
                AdapterErrorKind::ManagerConflict,
                format!("the Service Control Manager refused the request (error {code})"),
            ),
        };
        AdapterError::new(kind, &detail)
    }
}

impl fmt::Display for ScmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SCM error {}", self.code)
    }
}

impl std::error::Error for ScmError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_documented_state() {
        let expected = [
            (1, OsState::Disconnected),
            (2, OsState::Transitioning),
            (3, OsState::Transitioning),
            (4, OsState::Connected),
            (5, OsState::Transitioning),
            (6, OsState::Transitioning),
            (7, OsState::Unknown),
        ];
        for (raw, os_state) in expected {
            assert_eq!(
                ServiceState::from_raw(raw).os_state().unwrap(),
                os_state,
                "{raw}"
            );
        }
    }

    #[test]
    fn undocumented_states_are_parse_errors_not_guesses() {
        for raw in [0, 8, u32::MAX] {
            let error = ServiceState::from_raw(raw).os_state().unwrap_err();
            assert_eq!(error.kind, AdapterErrorKind::ParseError, "{raw}");
        }
    }

    #[test]
    fn a_missing_service_is_a_lost_binding_not_a_stopped_tunnel() {
        for gone in [
            ScmError::SERVICE_DOES_NOT_EXIST,
            ScmError::SERVICE_MARKED_FOR_DELETE,
        ] {
            assert_eq!(gone.to_adapter_error().kind, AdapterErrorKind::NotFound);
        }
    }

    #[test]
    fn maps_refusals_by_cause() {
        let cases = [
            (ScmError::ACCESS_DENIED, AdapterErrorKind::PermissionDenied),
            (ScmError::SERVICE_REQUEST_TIMEOUT, AdapterErrorKind::Timeout),
            (
                ScmError::SERVICE_DISABLED,
                AdapterErrorKind::ManagerConflict,
            ),
            (
                ScmError::SERVICE_DATABASE_LOCKED,
                AdapterErrorKind::ManagerConflict,
            ),
            (
                ScmError::SERVICE_CANNOT_ACCEPT_CTRL,
                AdapterErrorKind::ManagerConflict,
            ),
            (
                ScmError::SERVICE_DEPENDENCY_FAIL,
                AdapterErrorKind::ManagerConflict,
            ),
            (ScmError { code: 31 }, AdapterErrorKind::ManagerConflict),
        ];
        for (error, kind) in cases {
            assert_eq!(error.to_adapter_error().kind, kind, "{error}");
        }
    }
}
