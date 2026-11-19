//! `wgal-mcp`: WGAL's MCP server (owner decision of 2026-11-04). The user's
//! agent starts it and talks to it over stdio, one JSON-RPC message per line
//! (the Model Context Protocol's stdio transport). It opens no port and runs
//! no command: each tool is one or a few of the watchdog's own requests,
//! sent through the watchdog's pipe as the window sends them, marked as
//! coming from MCP. The watchdog answers them only while MCP is on in the
//! window, checks the account and the access list as for the window, and
//! writes each change to its log as the agent's. Nothing here ever says the
//! Bot Token, which an agent may only replace or clear, and the Chat ID only
//! while the window lets agents set Telegram (owner decision of
//! 2026-11-06).

use std::io::{self, BufRead, Read, Write};

mod rpc;
mod tools;

/// Longest line read from the agent: far above any valid message.
const MAX_LINE_BYTES: usize = 1024 * 1024;

/// This build's version as the window reads it from the installed
/// executable without running it: `wgal_protocol::MCP_VERSION_MARK`, the
/// version and a NUL byte (owner request of 2026-11-04). `--version` prints
/// it from these very bytes, so the whole mark stays in the executable.
const VERSION_MARK: &str = concat!("WGAL-MCP-VERSION=", env!("CARGO_PKG_VERSION"), "\0");

/// The version `VERSION_MARK` carries, cut from it here without a second
/// copy of the mark, so the executable holds the mark once.
fn marked_version() -> &'static str {
    let mark = std::hint::black_box(VERSION_MARK);
    mark.split_once('=')
        .map_or("", |(_, version)| version.trim_end_matches('\0'))
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("wgal-mcp {}", marked_version());
        return;
    }
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut server = rpc::Server::new(wgal_client::PipeWatchdog::new(wgal_client::Client::Mcp));
    let mut input = stdin.lock();
    let mut line = Vec::new();
    loop {
        line.clear();
        let answer = match read_line(&mut input, &mut line) {
            Ok(Line::Read) => server.handle_line(&line),
            Ok(Line::TooLong) => Some(rpc::too_long(MAX_LINE_BYTES)),
            Ok(Line::End) | Err(_) => return,
        };
        let Some(answer) = answer else {
            continue;
        };
        if writeln!(stdout, "{answer}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            return;
        }
    }
}

/// What [`read_line`] found.
#[derive(Debug, PartialEq, Eq)]
enum Line {
    /// A line, in the buffer without its line break.
    Read,
    /// A line longer than [`MAX_LINE_BYTES`]: skipped, and the buffer left
    /// empty.
    TooLong,
    /// The end of input.
    End,
}

/// Reads one line, without its line break (LF or CRLF), into `line`,
/// keeping at most [`MAX_LINE_BYTES`] of content and its line break: the
/// rest of a longer line is skipped up to its line break a buffer at a
/// time, never kept, so no line can make this process hold more (Codex
/// P2-N23). The limit is the content's, whichever the line break or the end
/// of input (Codex P3-N34).
fn read_line<R: BufRead>(input: &mut R, line: &mut Vec<u8>) -> io::Result<Line> {
    // The content, then a carriage return and a line feed.
    let most = MAX_LINE_BYTES + 2;
    let read = Read::take(&mut *input, most as u64).read_until(b'\n', line)?;
    if read == 0 {
        return Ok(Line::End);
    }
    if line.last() != Some(&b'\n') && line.len() == most {
        // No line feed within the content, a carriage return and a line
        // feed past the limit: too long, whatever follows.
        line.clear();
        skip_line(input)?;
        return Ok(Line::TooLong);
    }
    // Only a line feed, and one carriage return right before it, end a line;
    // a carriage return before the end of input is content (Codex P3-N37).
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    }
    if line.len() > MAX_LINE_BYTES {
        // Read whole, up to its line break or the end of input.
        line.clear();
        return Ok(Line::TooLong);
    }
    Ok(Line::Read)
}

/// Skips input up to and with the next line break, or to the end.
fn skip_line<R: BufRead>(input: &mut R) -> io::Result<()> {
    loop {
        let buffer = input.fill_buf()?;
        if buffer.is_empty() {
            return Ok(());
        }
        if let Some(at) = buffer.iter().position(|&b| b == b'\n') {
            input.consume(at + 1);
            return Ok(());
        }
        let all = buffer.len();
        input.consume(all);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::io::{BufReader, Cursor, Read};

    use super::{Line, MAX_LINE_BYTES, VERSION_MARK, marked_version, read_line};

    /// The mark the window looks for carries exactly this build's version.
    #[test]
    fn the_version_mark_carries_this_builds_version() {
        assert!(VERSION_MARK.starts_with(wgal_protocol::MCP_VERSION_MARK));
        assert!(VERSION_MARK.ends_with('\0'));
        assert_eq!(marked_version(), env!("CARGO_PKG_VERSION"));
        // As the window reads it.
        let read = wgal_protocol::mcp_version_in(VERSION_MARK.as_bytes()).unwrap();
        assert_eq!(read.as_str(), env!("CARGO_PKG_VERSION"));
    }

    /// Codex P2-N23: a line over the limit is skipped without being kept,
    /// however long it is, and the next line is read as usual.
    #[test]
    fn an_over_long_line_is_skipped_and_the_next_one_read() {
        let mut input = vec![b'x'; 5 * MAX_LINE_BYTES];
        input.extend_from_slice(b"\n{\"ok\":1}\r\n");
        // A small buffer, as stdin may hand over any amount at a time.
        let mut reader = BufReader::with_capacity(4096, Cursor::new(input));
        let mut line = Vec::new();
        assert_eq!(read_line(&mut reader, &mut line).unwrap(), Line::TooLong);
        assert!(line.is_empty());
        assert!(line.capacity() <= 2 * (MAX_LINE_BYTES + 1), "kept the rest");
        line.clear();
        assert_eq!(read_line(&mut reader, &mut line).unwrap(), Line::Read);
        assert_eq!(line, b"{\"ok\":1}");
        line.clear();
        assert_eq!(read_line(&mut reader, &mut line).unwrap(), Line::End);
    }

    /// Content of exactly the limit is read and one byte more is not,
    /// whether the line ends in LF, CRLF or the end of input (Codex
    /// P3-N34); either way the next line is read as usual.
    #[test]
    fn the_limit_is_exact_for_every_line_end() {
        for end in [&b"\n"[..], b"\r\n", b""] {
            for (extra, expected) in [(0, Line::Read), (1, Line::TooLong)] {
                let mut input = vec![b'x'; MAX_LINE_BYTES + extra];
                input.extend_from_slice(end);
                if !end.is_empty() {
                    input.extend_from_slice(b"{}\n");
                }
                let mut reader = BufReader::with_capacity(4096, Cursor::new(input));
                let mut line = Vec::new();
                let what = (end, extra);
                assert_eq!(
                    read_line(&mut reader, &mut line).unwrap(),
                    expected,
                    "{what:?}"
                );
                if expected == Line::Read {
                    assert_eq!(line.len(), MAX_LINE_BYTES, "{what:?}");
                } else {
                    assert!(line.is_empty(), "{what:?}");
                }
                line.clear();
                if end.is_empty() {
                    assert_eq!(read_line(&mut reader, &mut line).unwrap(), Line::End);
                } else {
                    assert_eq!(read_line(&mut reader, &mut line).unwrap(), Line::Read);
                    assert_eq!(line, b"{}", "{what:?}");
                }
                let mut rest = Vec::new();
                reader.read_to_end(&mut rest).unwrap();
                assert!(rest.is_empty(), "{what:?}");
            }
        }
    }

    /// Codex P3-N37: a carriage return right before the end of input is
    /// content, not a line break, so it counts towards the limit.
    #[test]
    fn a_bare_carriage_return_at_the_end_of_input_is_content() {
        let mut input = vec![b'x'; MAX_LINE_BYTES];
        input.push(b'\r');
        let mut reader = BufReader::new(Cursor::new(input));
        let mut line = Vec::new();
        assert_eq!(read_line(&mut reader, &mut line).unwrap(), Line::TooLong);
        assert_eq!(read_line(&mut reader, &mut line).unwrap(), Line::End);
        let mut reader = BufReader::new(Cursor::new(b"{}\r".to_vec()));
        let mut line = Vec::new();
        assert_eq!(read_line(&mut reader, &mut line).unwrap(), Line::Read);
        assert_eq!(line, b"{}\r", "kept: JSON reads it as white space");
    }

    /// Input that ends in the middle of an over-long line ends cleanly.
    #[test]
    fn an_over_long_line_at_the_end_of_input_ends_it() {
        let mut reader = BufReader::new(Cursor::new(vec![b'x'; MAX_LINE_BYTES + 10]));
        let mut line = Vec::new();
        assert_eq!(read_line(&mut reader, &mut line).unwrap(), Line::TooLong);
        assert_eq!(read_line(&mut reader, &mut line).unwrap(), Line::End);
    }
}
