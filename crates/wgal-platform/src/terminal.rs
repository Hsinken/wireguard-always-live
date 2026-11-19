//! Typing a secret at the terminal without it being shown: `wgal notify
//! token set` asks for the Bot Token this way (owner decision of
//! 2026-11-18). [`HiddenInput`] turns the echo of standard input's
//! terminal off while it lives and puts the mode back when it ends, on any
//! path, and also when the program is interrupted meanwhile (Ctrl+C and the
//! like), so the terminal is not left without echo. Reading the line is the
//! caller's; this only hides it.
//!
//! The echo is turned off under the same lock the interrupt path takes, so
//! an interrupt never puts the mode back before the echo is off, which
//! would leave it off once the program ends (Codex review of the CLI,
//! P2-6).
//!
//! On Linux and macOS the mode is the terminal's termios, through rustix's
//! safe API. SIGINT, SIGTERM, SIGHUP and SIGQUIT are caught by a thread
//! started with the first guard: it puts the mode back, if a guard lives,
//! and then handles the signal as the default would
//! (`emulate_default_handler`), so the program ends as it would have, with
//! a shell seeing 128 and the signal's number. The thread stays for the
//! rest of the program, which only takes back its default handling without
//! the `unsafe` a `sigaction` call needs: once the guard is gone a signal
//! does exactly what it would without it. SIGKILL cannot be caught. On
//! Windows it is the console's input mode, through the safe wrappers in
//! `windows/sys.rs`: the control handler is added when the echo goes off
//! and removed when it comes back; an interrupt meanwhile puts the mode
//! back and leaves the rest to Windows' default handling, which ends the
//! program.

use std::io;

/// While it lives, what is typed on standard input's terminal is not
/// shown. Dropping it puts the terminal's mode back.
#[derive(Debug)]
pub struct HiddenInput {
    /// Puts the mode back when dropped.
    _hidden: imp::Hidden,
}

impl HiddenInput {
    /// Turns the echo of standard input's terminal off. Fails, changing
    /// nothing, when standard input is not a terminal, or while another
    /// `HiddenInput` lives.
    pub fn begin() -> io::Result<Self> {
        imp::on_stdin().map(|hidden| Self { _hidden: hidden })
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod imp {
    use std::io;
    use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use rustix::termios::{self, LocalModes, OptionalActions, Termios};
    use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
    use signal_hook::iterator::Signals;
    use signal_hook::low_level::emulate_default_handler;

    /// The terminal whose echo is off and the mode to put back on it.
    type Saved = (OwnedFd, Termios);

    /// What the guard and the interrupt thread share.
    struct State {
        /// What to put back while a secret is typed.
        saved: Option<Saved>,
        /// An interrupt is ending the program: no echo may go off any
        /// more, so none can go off after the mode was put back (Codex
        /// review of the CLI, second round, P2-6).
        ending: bool,
    }

    static SAVED: Mutex<State> = Mutex::new(State {
        saved: None,
        ending: false,
    });

    /// The thread that puts the mode back on an interrupt: started with the
    /// first secret and kept for the life of the program, handling each
    /// signal as the default would.
    static WATCH: OnceLock<Result<(), io::ErrorKind>> = OnceLock::new();

    fn saved() -> MutexGuard<'static, State> {
        SAVED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Echo is off on [`SAVED`]'s terminal while this lives.
    #[derive(Debug)]
    pub(super) struct Hidden(());

    pub(super) fn on_stdin() -> io::Result<Hidden> {
        on(io::stdin().as_fd())
    }

    /// Turns echo off on `fd`'s terminal.
    pub(super) fn on(fd: BorrowedFd<'_>) -> io::Result<Hidden> {
        // Fails (ENOTTY) when `fd` is not a terminal, before anything
        // changes.
        let mode = termios::tcgetattr(fd)?;
        watch()?;
        let mut hidden = mode.clone();
        hidden.local_modes.remove(LocalModes::ECHO);
        // The echo goes off and the mode is saved under the lock the
        // interrupt thread takes: it puts the mode back after this, never
        // before.
        let mut state = saved();
        if state.ending {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        if state.saved.is_some() {
            return Err(io::Error::other("a secret is already being typed"));
        }
        let kept = fd.try_clone_to_owned()?;
        termios::tcsetattr(fd, OptionalActions::Now, &hidden)?;
        state.saved = Some((kept, mode));
        Ok(Hidden(()))
    }

    /// Puts the saved mode back, if any; nothing is saved afterwards.
    fn put_back() {
        restore(&mut saved());
    }

    fn restore(state: &mut State) {
        if let Some((fd, mode)) = state.saved.take() {
            let _ = termios::tcsetattr(&fd, OptionalActions::Now, &mode);
        }
    }

    /// What the interrupt thread does first: from now on no echo goes off,
    /// and the mode is put back if it is off, both under one lock, so a
    /// guard being made meanwhile either finished first (and its mode is
    /// put back here) or is refused.
    fn interrupted() {
        let mut state = saved();
        state.ending = true;
        restore(&mut state);
    }

    impl Drop for Hidden {
        fn drop(&mut self) {
            put_back();
        }
    }

    /// Starts, once, the thread that waits for an interrupt, puts the mode
    /// back and ends the program as the signal would have.
    fn watch() -> io::Result<()> {
        let started = WATCH.get_or_init(|| {
            let mut signals =
                Signals::new([SIGINT, SIGTERM, SIGHUP, SIGQUIT]).map_err(|e| e.kind())?;
            std::thread::Builder::new()
                .name("wgal-terminal".to_owned())
                .spawn(move || {
                    if let Some(signal) = signals.forever().next() {
                        interrupted();
                        // As the signal would have without this thread;
                        // the exit is only for a signal it cannot emulate.
                        let _ = emulate_default_handler(signal);
                        std::process::exit(128 + signal);
                    }
                })
                .map(|_| ())
                .map_err(|e| e.kind())
        });
        started.map_err(io::Error::from)
    }

    #[cfg(test)]
    mod tests {
        #![allow(clippy::unwrap_used)]

        use std::fs::{File, OpenOptions};
        use std::os::fd::AsFd;
        use std::os::unix::process::ExitStatusExt;
        use std::process::{Command, ExitStatus, Stdio};
        use std::sync::Mutex;

        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
        use rustix::termios::{self, LocalModes};

        use super::on;

        /// One pseudo-terminal at a time: [`super::SAVED`] holds one.
        static ONE: Mutex<()> = Mutex::new(());

        /// A pseudo-terminal of the test's own: its controller and the
        /// terminal end, which starts with echo on.
        fn pty() -> (std::os::fd::OwnedFd, File) {
            let controller = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
            grantpt(&controller).unwrap();
            unlockpt(&controller).unwrap();
            let name = ptsname(&controller, Vec::new()).unwrap();
            let terminal = OpenOptions::new()
                .read(true)
                .write(true)
                .open(name.to_str().unwrap())
                .unwrap();
            (controller, terminal)
        }

        fn echoes(terminal: &File) -> bool {
            termios::tcgetattr(terminal.as_fd())
                .unwrap()
                .local_modes
                .contains(LocalModes::ECHO)
        }

        /// Echo is off while the guard lives, and the whole mode is as it
        /// was once it is dropped; a second guard meanwhile is refused and
        /// changes nothing.
        #[test]
        fn echo_is_off_while_it_lives_and_back_after() {
            let _one = ONE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (_controller, terminal) = pty();
            let before = termios::tcgetattr(terminal.as_fd()).unwrap();
            assert!(echoes(&terminal));
            let hidden = on(terminal.as_fd()).unwrap();
            assert!(!echoes(&terminal));
            assert!(on(terminal.as_fd()).is_err(), "one at a time");
            assert!(!echoes(&terminal), "the refused one changed nothing");
            drop(hidden);
            assert!(echoes(&terminal));
            let after = termios::tcgetattr(terminal.as_fd()).unwrap();
            assert_eq!(after.local_modes, before.local_modes);
            assert_eq!(after.input_modes, before.input_modes);
            assert_eq!(after.output_modes, before.output_modes);
            // And again, as the next secret would.
            let hidden = on(terminal.as_fd()).unwrap();
            assert!(!echoes(&terminal));
            drop(hidden);
            assert!(echoes(&terminal));
        }

        /// Something that is not a terminal is refused, changing nothing.
        #[test]
        fn a_file_that_is_not_a_terminal_is_refused() {
            let _one = ONE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let file = File::open("/dev/null").unwrap();
            assert!(on(file.as_fd()).is_err());
            assert!(super::saved().saved.is_none());
        }

        /// The child half of the next tests: hides its standard input (the
        /// parent's pseudo-terminal), drops the guard first when asked,
        /// then is sent SIGINT. Does nothing unless the parent started it.
        #[test]
        fn child_is_interrupted() {
            let Some(how) = std::env::var_os("WGAL_TERMINAL_TEST_CHILD") else {
                return;
            };
            let hidden = super::on_stdin().unwrap();
            if how == "after" {
                drop(hidden);
            } else {
                std::mem::forget(hidden);
            }
            rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::INT)
                .unwrap();
            // The signal ends the program; it never gets here.
            std::thread::sleep(std::time::Duration::from_secs(10));
            std::process::exit(3);
        }

        /// The child half of the next test: an interrupt has begun, then a
        /// guard is asked for: refused, and the echo stays on. Exits 0
        /// when it does, so the parent can tell.
        #[test]
        fn child_hides_after_an_interrupt_began() {
            if std::env::var_os("WGAL_TERMINAL_TEST_ENDING").is_none() {
                return;
            }
            super::interrupted();
            let refused = matches!(
                super::on_stdin(),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted
            );
            let echo = termios::tcgetattr(std::io::stdin().as_fd())
                .is_ok_and(|t| t.local_modes.contains(LocalModes::ECHO));
            std::process::exit(if refused && echo { 0 } else { 5 });
        }

        /// Codex review of the CLI, second round, P2-6: once an interrupt
        /// has put the mode back, no guard turns the echo off again before
        /// the program ends.
        #[test]
        fn no_echo_goes_off_once_an_interrupt_began() {
            let _one = ONE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (_controller, terminal) = pty();
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "terminal::imp::tests::child_hides_after_an_interrupt_began",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env("WGAL_TERMINAL_TEST_ENDING", "1")
                .stdin(Stdio::from(terminal.try_clone().unwrap()))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(0), "{status:?}");
            assert!(echoes(&terminal));
        }

        /// Runs [`child_is_interrupted`] on `terminal`, `how`: `while` the
        /// guard lives, or `after` it was dropped.
        fn interrupted_child(terminal: &File, how: &str) -> ExitStatus {
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "terminal::imp::tests::child_is_interrupted",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env("WGAL_TERMINAL_TEST_CHILD", how)
                .stdin(Stdio::from(terminal.try_clone().unwrap()))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
        }

        /// Ctrl+C while a secret is typed: the terminal echoes again, and
        /// the program ends by SIGINT as it would without the guard (a
        /// shell sees 130).
        #[test]
        fn an_interrupt_puts_the_echo_back() {
            let _one = ONE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (_controller, terminal) = pty();
            assert!(echoes(&terminal));
            let status = interrupted_child(&terminal, "while");
            assert_eq!(status.signal(), Some(libc_sigint()), "{status:?}");
            assert!(echoes(&terminal), "echo was put back");
        }

        /// Once the guard is gone, SIGINT does what it does by default: the
        /// program ends by it, and the terminal is as it was (Codex review
        /// of the CLI, P2-8).
        #[test]
        fn after_the_guard_an_interrupt_is_the_default_one() {
            let _one = ONE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (_controller, terminal) = pty();
            let status = interrupted_child(&terminal, "after");
            assert_eq!(status.signal(), Some(libc_sigint()), "{status:?}");
            assert!(echoes(&terminal));
        }

        fn libc_sigint() -> i32 {
            signal_hook::consts::SIGINT
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::io;

    use crate::windows::{Win32Error, hide_stdin_echo, restore_stdin_echo};

    /// Echo is off on standard input's console while this lives.
    #[derive(Debug)]
    pub(super) struct Hidden(());

    fn io_error(error: Win32Error) -> io::Error {
        io::Error::from_raw_os_error(i32::try_from(error.code).unwrap_or(i32::MAX))
    }

    pub(super) fn on_stdin() -> io::Result<Hidden> {
        hide_stdin_echo().map_err(io_error)?;
        Ok(Hidden(()))
    }

    impl Drop for Hidden {
        fn drop(&mut self) {
            restore_stdin_echo();
        }
    }

    #[cfg(test)]
    mod tests {
        use crate::windows::console_mode_without_echo;

        /// Only the echo goes; line input, processed input and the rest
        /// stay as they were.
        #[test]
        fn only_the_echo_is_turned_off() {
            // ENABLE_PROCESSED_INPUT, ENABLE_LINE_INPUT, ENABLE_ECHO_INPUT,
            // ENABLE_INSERT_MODE, ENABLE_QUICK_EDIT_MODE,
            // ENABLE_EXTENDED_FLAGS: a usual console's mode.
            let usual = 0x0001 | 0x0002 | 0x0004 | 0x0020 | 0x0040 | 0x0080;
            assert_eq!(console_mode_without_echo(usual), usual & !0x0004);
            assert_eq!(console_mode_without_echo(0x0004), 0);
            assert_eq!(console_mode_without_echo(0x0003), 0x0003);
        }
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod imp {
    use std::io;

    #[derive(Debug)]
    pub(super) struct Hidden;

    pub(super) fn on_stdin() -> io::Result<Hidden> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "typing a secret unseen works only on Windows, Linux and macOS",
        ))
    }
}
