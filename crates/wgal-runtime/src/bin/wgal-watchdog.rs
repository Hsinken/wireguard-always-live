//! The watchdog executable. Each platform's service manager starts it: on
//! Windows the service control manager runs it as WGAL's own service, on
//! Linux systemd as root (`wgal-watchdog.service`), and on macOS launchd as
//! the signed-in user's LaunchAgent. It takes no paths, commands or service
//! names from its arguments.

use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(windows)]
    {
        wgal_runtime::windows::main()
    }
    #[cfg(target_os = "linux")]
    {
        wgal_runtime::linux::main()
    }
    #[cfg(target_os = "macos")]
    {
        wgal_runtime::macos::main()
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        eprintln!("wgal-watchdog: there is no background service for this platform");
        ExitCode::FAILURE
    }
}
