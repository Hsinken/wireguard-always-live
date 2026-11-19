//! Bounded runs of fixed OS tools (AGENTS.md rule 8).
//!
//! Every subprocess WGAL starts goes through a [`Runner`]: a [`Program`] fixed
//! at compile time, an argument vector the caller has already validated, no
//! shell, no inherited environment, no stdin, a cap on how much output is kept
//! and a deadline after which the process is killed and reaped. Adapters take
//! a `Runner` so tests can substitute [`ScriptedRunner`] and assert the exact
//! argument vectors without starting anything.

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Read};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use wgal_core::AdapterError;
use wgal_protocol::AdapterErrorKind;

/// How long to wait for the output readers after the process has exited. A
/// reader still open after this is held by a process the tool left behind.
const READER_GRACE: Duration = Duration::from_millis(500);

/// An OS tool WGAL may run, by absolute path. Construct it in a `const` so a
/// relative path fails to compile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Program(&'static str);

impl Program {
    /// Panics (at compile time when used in a `const`) unless `path` is
    /// absolute: `/...` or `X:\...`.
    pub const fn new(path: &'static str) -> Self {
        let b = path.as_bytes();
        let unix = !b.is_empty() && b[0] == b'/';
        let windows = b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
        assert!(unix || windows, "Program path must be absolute");
        Self(path)
    }

    pub const fn path(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// One run of a [`Program`]. `args` must already be validated: they are passed
/// as separate arguments, never through a shell, but the tool still parses
/// them. Each of stdout and stderr keeps at most `max_output_bytes`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub program: Program,
    pub args: Vec<String>,
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

/// What a finished process left. `exit_code` is `None` if a signal ended it.
/// `truncated` is set when either stream had more than `max_output_bytes`;
/// parsers must treat truncated output as unparseable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Output {
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
}

impl Output {
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }
}

/// Why a tool could not be run to completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecError {
    /// The program does not exist at its fixed path.
    NotFound,
    PermissionDenied,
    /// The deadline passed. The process was killed and reaped.
    Timeout,
    /// Any other OS error while starting or waiting for the process.
    Io(io::ErrorKind),
}

impl ExecError {
    /// The adapter error for a tool that could not be run. A missing or
    /// unusable tool is `Unsupported`, never `NotFound`: `NotFound` from an
    /// adapter means the tunnel or interface is gone, and a broken tool must
    /// not look like that (plan §4.1, T23).
    pub fn to_adapter_error(self, program: Program) -> AdapterError {
        let (kind, what) = match self {
            Self::NotFound => (AdapterErrorKind::Unsupported, "is not installed"),
            Self::PermissionDenied => (AdapterErrorKind::PermissionDenied, "is not permitted"),
            Self::Timeout => (AdapterErrorKind::Timeout, "did not finish in time"),
            Self::Io(_) => (AdapterErrorKind::Unsupported, "could not be run"),
        };
        AdapterError::new(kind, &format!("{program} {what}"))
    }
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("program not found"),
            Self::PermissionDenied => f.write_str("permission denied"),
            Self::Timeout => f.write_str("deadline passed"),
            Self::Io(kind) => write!(f, "I/O error: {kind}"),
        }
    }
}

impl std::error::Error for ExecError {}

/// Runs fixed OS tools. Implemented by [`SystemRunner`] and, in tests, by
/// [`ScriptedRunner`].
pub trait Runner: Send + Sync + fmt::Debug {
    fn run(&self, invocation: &Invocation) -> Result<Output, ExecError>;
}

/// The real [`Runner`]. The child gets no stdin and an empty environment
/// except `LC_ALL=C` and `LANG=C` (so output is not localized) and, on
/// Windows, `SystemRoot`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, invocation: &Invocation) -> Result<Output, ExecError> {
        let started = Instant::now();
        let mut command = Command::new(invocation.program.path());
        command
            .args(&invocation.args)
            .env_clear()
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }

        let mut child = command.spawn().map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => ExecError::NotFound,
            io::ErrorKind::PermissionDenied => ExecError::PermissionDenied,
            kind => ExecError::Io(kind),
        })?;
        let cap = invocation.max_output_bytes;
        let stdout = child.stdout.take().map(|pipe| spawn_reader(pipe, cap));
        let stderr = child.stderr.take().map(|pipe| spawn_reader(pipe, cap));

        let exit_code = wait_until(&mut child, started, invocation.timeout)?;
        let (stdout, stdout_truncated) = collect(stdout)?;
        let (stderr, stderr_truncated) = collect(stderr)?;
        Ok(Output {
            exit_code,
            stdout,
            stderr,
            truncated: stdout_truncated || stderr_truncated,
        })
    }
}

type Captured = (Vec<u8>, bool);

/// Keeps the first `cap` bytes and drains the rest, so a chatty tool cannot
/// block on a full pipe or grow memory.
fn spawn_reader(mut pipe: impl Read + Send + 'static, cap: usize) -> Receiver<Captured> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut kept = Vec::new();
        let mut truncated = false;
        let mut chunk = [0_u8; 4096];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    let room = cap.saturating_sub(kept.len());
                    kept.extend_from_slice(&chunk[..n.min(room)]);
                    truncated |= n > room;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    truncated = true;
                    break;
                }
            }
        }
        // The receiver is gone only if the run already failed.
        let _ = tx.send((kept, truncated));
    });
    rx
}

fn collect(reader: Option<Receiver<Captured>>) -> Result<Captured, ExecError> {
    match reader {
        Some(rx) => rx
            .recv_timeout(READER_GRACE)
            .map_err(|_| ExecError::Timeout),
        None => Ok((Vec::new(), false)),
    }
}

/// Waits for the child until `timeout` after `started`, then kills and reaps it.
fn wait_until(
    child: &mut Child,
    started: Instant,
    timeout: Duration,
) -> Result<Option<i32>, ExecError> {
    let mut pause = Duration::from_millis(1);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.code()),
            Ok(None) => {}
            Err(e) => {
                kill_and_reap(child);
                return Err(ExecError::Io(e.kind()));
            }
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            kill_and_reap(child);
            return Err(ExecError::Timeout);
        }
        thread::sleep(pause.min(timeout - elapsed));
        pause = (pause * 2).min(Duration::from_millis(20));
    }
}

fn kill_and_reap(child: &mut Child) {
    // Either can fail only if the process already exited, which is the goal.
    let _ = child.kill();
    let _ = child.wait();
}

/// Test double for [`Runner`]: answers with queued replies in order and
/// records every invocation. It never starts a process. Running it with no
/// reply queued panics, since that is a mistake in the test.
#[derive(Debug, Default)]
pub struct ScriptedRunner {
    state: Mutex<Script>,
}

#[derive(Debug, Default)]
struct Script {
    replies: VecDeque<Result<Output, ExecError>>,
    calls: Vec<Invocation>,
}

impl ScriptedRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, reply: Result<Output, ExecError>) {
        self.lock().replies.push_back(reply);
    }

    /// Queues a run that exits with `exit_code` and prints `stdout`.
    pub fn push_output(&self, exit_code: i32, stdout: &str) {
        self.push(Ok(Output {
            exit_code: Some(exit_code),
            stdout: stdout.as_bytes().to_vec(),
            ..Output::default()
        }));
    }

    /// Every invocation so far, in order.
    pub fn calls(&self) -> Vec<Invocation> {
        self.lock().calls.clone()
    }

    /// Replies queued but not used yet.
    pub fn pending(&self) -> usize {
        self.lock().replies.len()
    }

    fn lock(&self) -> MutexGuard<'_, Script> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Runner for ScriptedRunner {
    fn run(&self, invocation: &Invocation) -> Result<Output, ExecError> {
        let mut script = self.lock();
        script.calls.push(invocation.clone());
        script
            .replies
            .pop_front()
            .unwrap_or_else(|| panic!("ScriptedRunner: no reply queued for {}", invocation.program))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation(program: Program, args: &[&str], timeout_ms: u64, cap: usize) -> Invocation {
        Invocation {
            program,
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            timeout: Duration::from_millis(timeout_ms),
            max_output_bytes: cap,
        }
    }

    #[cfg(windows)]
    mod tools {
        use super::Program;
        // Help text is long enough to truncate; a loopback ping is slow enough
        // to time out. Neither touches a tunnel.
        pub const TALKER: Program = Program::new(r"C:\Windows\System32\where.exe");
        pub const TALKER_ARGS: &[&str] = &["/?"];
        pub const SLOW: Program = Program::new(r"C:\Windows\System32\PING.EXE");
        pub const SLOW_ARGS: &[&str] = &["-n", "30", "127.0.0.1"];
        pub const MISSING: Program = Program::new(r"C:\wgal-no-such-dir\tool.exe");
    }

    #[cfg(unix)]
    mod tools {
        use super::Program;
        pub const TALKER: Program = Program::new("/bin/echo");
        pub const TALKER_ARGS: &[&str] = &["0123456789abcdefghijklmnopqrstuvwxyz"];
        pub const SLOW: Program = Program::new("/bin/sleep");
        pub const SLOW_ARGS: &[&str] = &["30"];
        pub const MISSING: Program = Program::new("/wgal-no-such-dir/tool");
    }

    #[test]
    fn keeps_output_up_to_the_cap_and_flags_the_rest() {
        let full = SystemRunner
            .run(&invocation(
                tools::TALKER,
                tools::TALKER_ARGS,
                10_000,
                1 << 20,
            ))
            .unwrap();
        assert!(full.success(), "{full:?}");
        assert!(!full.truncated);
        assert!(full.stdout.len() > 16);

        let capped = SystemRunner
            .run(&invocation(tools::TALKER, tools::TALKER_ARGS, 10_000, 16))
            .unwrap();
        assert_eq!(capped.stdout, full.stdout[..16]);
        assert!(capped.truncated);
    }

    #[test]
    fn kills_a_tool_that_outlives_its_deadline() {
        let started = Instant::now();
        let result = SystemRunner.run(&invocation(tools::SLOW, tools::SLOW_ARGS, 300, 4096));
        assert_eq!(result, Err(ExecError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_missing_tool_is_not_found_and_maps_to_unsupported() {
        let result = SystemRunner.run(&invocation(tools::MISSING, &[], 1_000, 16));
        assert_eq!(result, Err(ExecError::NotFound));
        let error = ExecError::NotFound.to_adapter_error(tools::MISSING);
        assert_eq!(error.kind, AdapterErrorKind::Unsupported);
    }

    #[test]
    fn scripted_runner_records_calls_and_replays_in_order() {
        let runner = ScriptedRunner::new();
        runner.push_output(0, "first");
        runner.push(Err(ExecError::Timeout));
        let call = invocation(tools::TALKER, &["a", "b c"], 1_000, 16);

        assert_eq!(runner.run(&call).unwrap().stdout, b"first");
        assert_eq!(runner.run(&call), Err(ExecError::Timeout));
        assert_eq!(runner.calls(), vec![call.clone(), call]);
        assert_eq!(runner.pending(), 0);
    }

    #[test]
    #[should_panic(expected = "no reply queued")]
    fn scripted_runner_panics_when_the_script_runs_out() {
        let _ = ScriptedRunner::new().run(&invocation(tools::TALKER, &[], 1_000, 16));
    }
}
