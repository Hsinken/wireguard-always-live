//! What `wgal` reads besides its arguments: a line typed at the terminal
//! (the Bot Token unseen, a confirmation), a line or the whole of standard
//! input, and the pause between two looks at the watchdog. Behind a trait,
//! so the tests give scripted input and wait for nothing.

use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::time::{Duration, Instant};

/// One line read, without its line break.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    /// The line's content.
    Read(Vec<u8>),
    /// A line longer than the limit: refused, never cut.
    TooLong,
    /// No line at all: the input ended first.
    End,
}

/// The whole of standard input.
#[derive(Debug, PartialEq, Eq)]
pub enum Input {
    Read(Vec<u8>),
    /// More than the limit: refused, never cut.
    TooLong,
}

pub trait Console {
    /// Whether standard input is a terminal.
    fn stdin_is_terminal(&self) -> bool;

    /// Whether standard input and standard output are both terminals.
    fn interactive(&self) -> bool;

    /// Says `prompt` on standard error, then reads one line typed at the
    /// terminal with its echo off, of at most `max` bytes. An interrupt
    /// meanwhile puts the echo back and ends the program.
    fn read_hidden_line(&mut self, prompt: &str, max: usize) -> io::Result<Line>;

    /// Says `prompt` on standard error, then reads one line typed at the
    /// terminal, of at most `max` bytes.
    fn ask(&mut self, prompt: &str, max: usize) -> io::Result<Line>;

    /// Reads one line of standard input, of at most `max` bytes.
    fn read_line(&mut self, max: usize) -> io::Result<Line>;

    /// Reads the rest of standard input, at most `max` bytes.
    fn read_all(&mut self, max: usize) -> io::Result<Input>;

    /// Waits `time` before looking at the watchdog again.
    fn pause(&mut self, time: Duration);

    /// How long the program has run, by a monotonic clock: what a wait is
    /// measured against.
    fn elapsed(&self) -> Duration;
}

/// Reads one line from `input`, without its line break (LF or CRLF), of at
/// most `max` bytes of content: a longer one is [`Line::TooLong`], and
/// nothing past the limit and its line break is read.
pub fn read_line_from<R: BufRead>(input: &mut R, max: usize) -> io::Result<Line> {
    let mut line = Vec::new();
    // The content, then a carriage return and a line feed.
    let most = max + 2;
    let read = Read::take(&mut *input, most as u64).read_until(b'\n', &mut line)?;
    if read == 0 {
        return Ok(Line::End);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    } else if line.len() == most {
        return Ok(Line::TooLong);
    }
    if line.len() > max {
        return Ok(Line::TooLong);
    }
    Ok(Line::Read(line))
}

/// Reads all of `input`, at most `max` bytes: one byte more is
/// [`Input::TooLong`], and nothing further is read.
pub fn read_all_from<R: Read>(input: &mut R, max: usize) -> io::Result<Input> {
    let mut all = Vec::new();
    Read::take(input, max as u64 + 1).read_to_end(&mut all)?;
    if all.len() > max {
        Ok(Input::TooLong)
    } else {
        Ok(Input::Read(all))
    }
}

/// The real terminal and standard input.
#[derive(Debug)]
pub struct Terminal {
    started: Instant,
}

impl Terminal {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }

    fn say(prompt: &str) -> io::Result<()> {
        let mut stderr = io::stderr().lock();
        stderr.write_all(prompt.as_bytes())?;
        stderr.flush()
    }
}

impl Console for Terminal {
    fn stdin_is_terminal(&self) -> bool {
        io::stdin().is_terminal()
    }

    fn interactive(&self) -> bool {
        io::stdin().is_terminal() && io::stdout().is_terminal()
    }

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    fn read_hidden_line(&mut self, prompt: &str, max: usize) -> io::Result<Line> {
        // The echo is off before the prompt asks for anything, so nothing
        // typed or pasted at once is shown (Codex review of the CLI,
        // P2-5); a failed prompt drops the guard, which puts it back.
        let hidden = wgal_platform::terminal::HiddenInput::begin()?;
        Self::say(prompt)?;
        let line = read_line_from(&mut io::stdin().lock(), max);
        // The echo is back before anything else is said.
        drop(hidden);
        // Enter was not shown either.
        Self::say("\n")?;
        line
    }

    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    fn read_hidden_line(&mut self, _prompt: &str, _max: usize) -> io::Result<Line> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            crate::text::tr!("console.hidden_input_unsupported"),
        ))
    }

    fn ask(&mut self, prompt: &str, max: usize) -> io::Result<Line> {
        Self::say(prompt)?;
        read_line_from(&mut io::stdin().lock(), max)
    }

    fn read_line(&mut self, max: usize) -> io::Result<Line> {
        read_line_from(&mut io::stdin().lock(), max)
    }

    fn read_all(&mut self, max: usize) -> io::Result<Input> {
        read_all_from(&mut io::stdin().lock(), max)
    }

    fn pause(&mut self, time: Duration) {
        std::thread::sleep(time);
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}
