//! Parsers for `wg show <interface> latest-handshakes` and `... transfer`.
//!
//! Each line starts with a peer's public key. Keys are checked for shape and
//! then dropped: they never reach a result or an error message. Only these two
//! subcommands are used; never `wg show all dump` or `wg showconf`, which
//! include private keys.

use wgal_core::{AdapterError, TunnelMetrics};
use wgal_protocol::AdapterErrorKind;

use crate::exec::Output;

/// Most bytes a `wg show` answer may have before it is refused.
pub const MAX_WG_BYTES: usize = 64 * 1024;

fn parse_error(detail: &str) -> AdapterError {
    AdapterError::new(AdapterErrorKind::ParseError, detail)
}

/// A WireGuard public key: 32 bytes in base64, so 43 characters and `=`.
fn is_public_key(field: &str) -> bool {
    field.len() == 44
        && field.ends_with('=')
        && field[..43]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}

/// The peer lines of a `wg show` answer, split on tabs, each with exactly
/// `fields` numeric columns after the key.
fn rows(stdout: &[u8], fields: usize) -> Result<Vec<Vec<u64>>, AdapterError> {
    let text = std::str::from_utf8(stdout).map_err(|_| parse_error("wg output is not UTF-8"))?;
    if !text.is_empty() && !text.ends_with('\n') {
        return Err(parse_error("wg output is incomplete"));
    }
    text.lines()
        .map(|line| {
            let mut columns = line.split('\t');
            if !columns.next().is_some_and(is_public_key) {
                return Err(parse_error("wg line does not start with a public key"));
            }
            let numbers: Vec<u64> = columns
                .map(|c| {
                    c.parse()
                        .map_err(|_| parse_error("wg column is not a number"))
                })
                .collect::<Result<_, _>>()?;
            if numbers.len() != fields {
                return Err(parse_error("wg line has the wrong number of columns"));
            }
            Ok(numbers)
        })
        .collect()
}

/// Combines both answers: the newest handshake over all peers (`None` if no
/// peer has completed one) and the summed byte counters.
pub fn parse_metrics(handshakes: &[u8], transfer: &[u8]) -> Result<TunnelMetrics, AdapterError> {
    let handshakes = rows(handshakes, 1)?;
    let transfer = rows(transfer, 2)?;
    if handshakes.len() != transfer.len() {
        return Err(parse_error("wg answers list different peers"));
    }
    let newest = handshakes.iter().map(|r| r[0]).max().filter(|&s| s > 0);
    let latest_handshake_unix_ms = newest
        .map(|s| {
            s.checked_mul(1000)
                .and_then(|ms| i64::try_from(ms).ok())
                .ok_or_else(|| parse_error("wg handshake time is out of range"))
        })
        .transpose()?;
    let sum = |column: usize| {
        transfer
            .iter()
            .try_fold(0_u64, |total, r| total.checked_add(r[column]))
            .ok_or_else(|| parse_error("wg transfer counters overflow"))
    };
    Ok(TunnelMetrics {
        latest_handshake_unix_ms,
        rx_bytes: Some(sum(0)?),
        tx_bytes: Some(sum(1)?),
    })
}

/// A `wg show` run that exited non-zero, classified from its (C locale) error
/// text without copying it.
pub fn classify_failure(output: &Output) -> AdapterError {
    let text = String::from_utf8_lossy(&output.stderr);
    if text.contains("Operation not permitted") || text.contains("Permission denied") {
        AdapterError::new(
            AdapterErrorKind::PermissionDenied,
            "reading WireGuard counters needs privileges",
        )
    } else if text.contains("No such device") {
        AdapterError::new(AdapterErrorKind::NotFound, "interface no longer exists")
    } else {
        parse_error("wg failed with an unrecognised error")
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::fixture;
    use super::*;

    #[test]
    fn newest_handshake_and_summed_counters_across_peers() {
        let metrics = parse_metrics(
            fixture("wg-latest-handshakes.txt").as_bytes(),
            fixture("wg-transfer.txt").as_bytes(),
        )
        .unwrap();
        assert_eq!(
            metrics,
            TunnelMetrics {
                latest_handshake_unix_ms: Some(1_767_225_590_000),
                rx_bytes: Some(1_500_000 + 2_048),
                tx_bytes: Some(700_000 + 1_024),
            }
        );
    }

    #[test]
    fn no_peers_or_no_handshake_yet() {
        let metrics = parse_metrics(b"", b"").unwrap();
        assert_eq!(metrics.latest_handshake_unix_ms, None);
        assert_eq!(metrics.rx_bytes, Some(0));
        let key = "ZmFrZS1wdWJsaWMta2V5LWZvci13Z2FsLXRlc3RzMDE=";
        let metrics = parse_metrics(
            format!("{key}\t0\n").as_bytes(),
            format!("{key}\t0\t0\n").as_bytes(),
        )
        .unwrap();
        assert_eq!(metrics.latest_handshake_unix_ms, None);
    }

    #[test]
    fn malformed_answers_fail_without_echoing_keys() {
        let key = "ZmFrZS1wdWJsaWMta2V5LWZvci13Z2FsLXRlc3RzMDE=";
        let bad = [
            (format!("{key}\t12\n"), format!("{key}\t1\n")),
            (format!("{key}\tx\n"), format!("{key}\t1\t2\n")),
            ("short\t12\n".to_owned(), "short\t1\t2\n".to_owned()),
            (format!("{key}\t12"), format!("{key}\t1\t2\n")),
            (format!("{key}\t12\n{key}\t13\n"), format!("{key}\t1\t2\n")),
            (format!("{key}\t{}\n", u64::MAX), format!("{key}\t1\t2\n")),
            (
                format!("{key}\t1\n{key}\t1\n"),
                format!("{key}\t{}\t2\n{key}\t1\t2\n", u64::MAX),
            ),
        ];
        for (handshakes, transfer) in bad {
            let error = parse_metrics(handshakes.as_bytes(), transfer.as_bytes()).unwrap_err();
            assert_eq!(error.kind, AdapterErrorKind::ParseError, "{handshakes:?}");
            assert!(!error.detail.as_str().contains(key));
        }
    }

    #[test]
    fn classifies_wg_failures() {
        let failed = |stderr: &str| Output {
            exit_code: Some(1),
            stderr: stderr.as_bytes().to_vec(),
            ..Output::default()
        };
        let cases = [
            (
                "Unable to access interface: Operation not permitted\n",
                AdapterErrorKind::PermissionDenied,
            ),
            (
                "Unable to access interface: No such device\n",
                AdapterErrorKind::NotFound,
            ),
            ("Invalid subcommand\n", AdapterErrorKind::ParseError),
        ];
        for (stderr, expected) in cases {
            assert_eq!(
                classify_failure(&failed(stderr)).kind,
                expected,
                "{stderr:?}"
            );
        }
    }
}
