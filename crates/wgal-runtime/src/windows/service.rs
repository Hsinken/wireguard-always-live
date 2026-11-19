//! The Windows service entry point (plan §3.1): the SCM starts
//! `wgal-watchdog.exe` as the service [`SERVICE_NAME`], running as
//! LocalSystem. Everything it runs is in [`super::host`]; this module only
//! talks to the SCM, through the `windows-service` crate.
//!
//! The executable takes no arguments that name a path, command, service or
//! pipe (AGENTS.md rule 8): `--version` prints the version, and anything else
//! prints what the program is for. Started from a console rather than by the
//! SCM, it says how it is meant to run and exits with an error.
//!
//! Status: `START_PENDING` with a wait hint while the parts are built, then
//! `RUNNING` accepting stop, shutdown, preshutdown and power events, then
//! `STOP_PENDING` with a checkpoint every few seconds while the loop stops,
//! then `STOPPED` with the exit code from [`super::host`].
//!
//! - Preshutdown is accepted because a plain shutdown notice leaves a
//!   service only the system's short kill timeout, while stopping the loop
//!   may have to wait for a stop or start already handed to the SCM (up to
//!   25 s) so that a tunnel WGAL stopped is started again and the state is
//!   saved. Preshutdown holds the shutdown until the service stops or the
//!   system's preshutdown timeout ends. A service that stops on preshutdown
//!   is not sent the shutdown notice as well.
//! - Resuming from sleep (`PBT_APMRESUMEAUTOMATIC`, `PBT_APMRESUMESUSPEND`)
//!   tells the loop to start its grace period again (plan §6.2, T15). Other
//!   power events are acknowledged and ignored.
//! - Network changes do the same: once running, the service registers for
//!   the IP Helper's IPv4 interface notices (`NotifyIpInterfaceChange`,
//!   wrapped in `wgal_platform`'s `sys.rs`) and passes changes on
//!   interfaces that are not tunnels, coalesced, to
//!   `Client::on_network_change` (see `super::network`). The registration
//!   is cancelled when the service stops. That this host itself is offline
//!   (open finding D-3) is told by the Windows adapter when a probe fails,
//!   not here.

use std::ffi::OsString;
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use windows_service::service::{
    PowerEventParam, ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState,
    ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{
    self, ServiceControlHandlerResult, ServiceStatusHandle,
};
use windows_service::{define_windows_service, service_dispatcher};

use wgal_platform::windows::WindowsAdapter;

use super::access::Protection;
use super::host::{self, Control, Finished, Layout, Limits};
use super::server::{Server, ServerLimits};

/// The WGAL service's name in the SCM. Fixed; the installer uses the same.
pub const SERVICE_NAME: &str = "WGALWatchdog";

/// Longest the SCM should wait for a start before it assumes the service
/// hung. Building the parts takes well under a second.
const START_WAIT: Duration = Duration::from_secs(30);
/// What a stop may take: the pipe server's limit plus the loop's deadline
/// and margin (10 + 30 + 5 s); `finish` never takes longer.
const STOP_WAIT: Duration = Duration::from_secs(45);

const HINT: &str = "wgal-watchdog is the WGAL background service. It is started by the Windows \
                    service manager as WGALWatchdog, which packaging\\windows\\install.ps1 sets \
                    up; it does nothing when started from a console.";

define_windows_service!(ffi_service_main, service_main);

/// The executable's entry point.
pub fn main() -> ExitCode {
    run(std::env::args_os().skip(1).collect())
}

fn run(arguments: Vec<OsString>) -> ExitCode {
    match arguments.as_slice() {
        [] => match service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
            Ok(()) => ExitCode::SUCCESS,
            // Most often ERROR_FAILED_SERVICE_CONTROLLER_CONNECT: not
            // started by the SCM.
            Err(_) => {
                eprintln!("{HINT}");
                ExitCode::FAILURE
            }
        },
        [only] if only == "--version" => {
            println!("wgal-watchdog {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{HINT} It takes no arguments other than --version.");
            ExitCode::FAILURE
        }
    }
}

/// Reports the service's state to the SCM.
struct Status {
    handle: ServiceStatusHandle,
    checkpoint: u32,
}

impl Status {
    fn set(&mut self, state: ServiceState, exit: Option<u32>, wait: Duration) {
        let pending = matches!(
            state,
            ServiceState::StartPending | ServiceState::StopPending
        );
        self.checkpoint = if pending { self.checkpoint + 1 } else { 0 };
        let controls_accepted = if state == ServiceState::Running {
            ServiceControlAccept::STOP
                | ServiceControlAccept::SHUTDOWN
                | ServiceControlAccept::PRESHUTDOWN
                | ServiceControlAccept::POWER_EVENT
        } else {
            ServiceControlAccept::empty()
        };
        let exit_code = exit.map_or(ServiceExitCode::NO_ERROR, ServiceExitCode::ServiceSpecific);
        // Nothing better can be done if the SCM does not take the report.
        let _ = self.handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted,
            exit_code,
            checkpoint: self.checkpoint,
            wait_hint: if pending { wait } else { Duration::ZERO },
            process_id: None,
        });
    }
}

/// What the SCM's handler does with each control: stop, shutdown and
/// preshutdown ask the service to stop, a resume wakes the loop. It only
/// passes messages on, so it returns at once.
fn on_control(controls: &Sender<Control>, control: ServiceControl) -> ServiceControlHandlerResult {
    match control {
        ServiceControl::Stop | ServiceControl::Shutdown | ServiceControl::Preshutdown => {
            let _ = controls.send(Control::Stop);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::PowerEvent(
            PowerEventParam::ResumeAutomatic | PowerEventParam::ResumeSuspend,
        ) => {
            let _ = controls.send(Control::Wake);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::PowerEvent(_) | ServiceControl::Interrogate => {
            ServiceControlHandlerResult::NoError
        }
        _ => ServiceControlHandlerResult::NotImplemented,
    }
}

/// Runs on the dispatcher's thread once the SCM starts the service. The
/// SCM's start parameters are ignored.
fn service_main(_arguments: Vec<OsString>) {
    let (controls, inbox) = mpsc::channel();
    let handler = controls.clone();
    let Ok(handle) = service_control_handler::register(SERVICE_NAME, move |control| {
        on_control(&handler, control)
    }) else {
        // Without a status handle nothing can be reported; the SCM sees the
        // service end without reaching RUNNING.
        return;
    };
    let mut status = Status {
        handle,
        checkpoint: 0,
    };
    status.set(ServiceState::StartPending, None, START_WAIT);
    let finished = serve(&mut status, controls, &inbox);
    status.set(ServiceState::Stopped, finished.exit_code, Duration::ZERO);
    if !finished.clean {
        // A thread of the pipe server or the loop is stuck. The SCM has the
        // final status; ending the process takes the stuck threads with it
        // instead of leaving a stopped service's process behind.
        std::process::exit(i32::from(finished.exit_code.is_some()));
    }
}

fn serve(status: &mut Status, controls: Sender<Control>, inbox: &Receiver<Control>) -> Finished {
    let failed = |exit_code| Finished {
        exit_code: Some(exit_code),
        clean: true,
    };
    let layout = match Layout::installation() {
        Ok(layout) => layout,
        Err(error) => return failed(error.exit_code()),
    };
    let mut hosted = match host::start(
        &layout,
        Protection::AdminOnly,
        WindowsAdapter::new(),
        host::transport(),
        |access| Server::watchdog(access, ServerLimits::DEFAULT),
        controls,
        Limits::DEFAULT,
    ) {
        Ok(hosted) => hosted,
        Err(error) => return failed(error.exit_code()),
    };
    hosted.watch_network_changes();
    status.set(ServiceState::Running, None, Duration::ZERO);
    let ending = hosted.watch(inbox);
    status.set(ServiceState::StopPending, None, STOP_WAIT);
    hosted.finish(ending, &mut || {
        status.set(ServiceState::StopPending, None, STOP_WAIT);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stop_wait_hint_covers_the_whole_bounded_stop() {
        assert!(STOP_WAIT >= host::SERVER_STOP + host::STOP_DEADLINE + host::STOP_MARGIN);
    }

    #[test]
    fn only_the_version_argument_is_taken() {
        assert_eq!(run(vec!["--version".into()]), ExitCode::SUCCESS);
        for other in [
            vec!["--install".into()],
            vec![r"C:\Windows\System32\cmd.exe".into()],
            vec!["--version".into(), "extra".into()],
            vec![r"\\.\pipe\elsewhere".into()],
        ] {
            assert_eq!(run(other), ExitCode::FAILURE);
        }
    }

    /// StartServiceCtrlDispatcherW from a process the SCM did not start
    /// fails at once (ERROR_FAILED_SERVICE_CONTROLLER_CONNECT); nothing is
    /// registered, installed or started.
    #[test]
    fn started_from_a_console_it_explains_itself_and_fails() {
        assert_eq!(run(Vec::new()), ExitCode::FAILURE);
    }

    #[test]
    fn the_control_handler_only_passes_stops_and_resumes_on() {
        let (controls, inbox) = mpsc::channel();
        for stop in [
            ServiceControl::Stop,
            ServiceControl::Shutdown,
            ServiceControl::Preshutdown,
        ] {
            assert!(matches!(
                on_control(&controls, stop),
                ServiceControlHandlerResult::NoError
            ));
            assert!(matches!(inbox.try_recv(), Ok(Control::Stop)));
        }
        for resume in [
            PowerEventParam::ResumeAutomatic,
            PowerEventParam::ResumeSuspend,
        ] {
            on_control(&controls, ServiceControl::PowerEvent(resume));
            assert!(matches!(inbox.try_recv(), Ok(Control::Wake)));
        }
        for quiet in [
            ServiceControl::PowerEvent(PowerEventParam::Suspend),
            ServiceControl::Interrogate,
        ] {
            assert!(matches!(
                on_control(&controls, quiet),
                ServiceControlHandlerResult::NoError
            ));
        }
        assert!(matches!(
            on_control(&controls, ServiceControl::Pause),
            ServiceControlHandlerResult::NotImplemented
        ));
        assert!(inbox.try_recv().is_err());
    }
}
