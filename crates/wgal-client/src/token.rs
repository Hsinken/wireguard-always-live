//! The shape of a Telegram Bot Token, so neither `wgal-mcp` nor `wgal`
//! repeats one, wherever it came from (AGENTS.md rule 5): `wgal-mcp` hides
//! it in anything it sends an agent (Codex review of 2026-11-06), and `wgal`
//! refuses a command line that holds one, since a token there stays in the
//! shell's history and other processes can see it (owner decision of
//! 2026-11-18).

/// What replaces text shaped like a Bot Token.
pub const HIDDEN_TOKEN: &str = "[hidden: shaped like a Bot Token]";

/// The byte spans of `text` shaped like a Telegram Bot Token: digits, `:`,
/// then 30 or more of `A-Z a-z 0-9 _ -`. Any number of digits counts, since
/// a longer run still ends in a bot ID (Codex review of 2026-11-06). Every
/// byte of a span is ASCII, so its ends are character boundaries.
fn token_spans(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut spans = Vec::new();
    for (i, c) in b.iter().enumerate() {
        if *c != b':' {
            continue;
        }
        let digits = b[..i]
            .iter()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .count();
        let secret = b[i + 1..]
            .iter()
            .take_while(|c| c.is_ascii_alphanumeric() || **c == b'_' || **c == b'-')
            .count();
        if digits >= 1 && secret >= 30 {
            spans.push((i - digits, i + 1 + secret));
        }
    }
    spans
}

/// Whether `text` holds something shaped like a Bot Token.
pub fn looks_like_token(text: &str) -> bool {
    !token_spans(text).is_empty()
}

/// `text` with everything shaped like a Bot Token replaced.
pub fn without_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end) in token_spans(text) {
        if start >= at {
            out.push_str(&text[at..start]);
            out.push_str(HIDDEN_TOKEN);
            at = end;
        } else if end > at {
            // Overlaps the span before: the digits are that span's tail.
            at = end;
        }
    }
    out.push_str(&text[at..]);
    out
}
