//! Bounded line splitting and the parser for the nested property-list
//! descriptions `scutil` prints:
//!
//! ```text
//! <dictionary> {
//!   IPv4 : <dictionary> {
//!     Addresses : <array> {
//!       0 : 192.0.2.10
//!     }
//!     InterfaceName : utun4
//!   }
//! }
//! ```
//!
//! Structure comes from the braces, and every line must also sit at exactly
//! two spaces per nesting level, so output from two runs interleaved, or cut
//! off, does not parse. Keys are identifiers, dictionaries may not repeat a
//! key, array indexes must count up from 0, and nesting, entries per
//! container, lines and line length are all capped.

use wgal_core::AdapterError;
use wgal_protocol::AdapterErrorKind;

/// Deepest nesting accepted. The extended status needs 3 (`IPv4`,
/// `Addresses`, an address).
pub const MAX_DEPTH: usize = 8;
pub const MAX_ENTRIES: usize = 64;
pub const MAX_LINES: usize = 1024;
pub const MAX_LINE_BYTES: usize = 512;
const MAX_KEY_BYTES: usize = 64;

/// Why output was rejected. Static text only, so no tool output, which may
/// name configurations or addresses, is ever copied into an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Malformed(pub &'static str);

impl Malformed {
    pub fn into_error(self, tool: &str) -> AdapterError {
        AdapterError::new(
            AdapterErrorKind::ParseError,
            &format!("unexpected {tool} output: {}", self.0),
        )
    }
}

/// Splits complete tool output into lines. The output must end with a
/// newline (otherwise it may have been cut off) and may not contain control
/// characters other than that newline, carriage returns included.
pub fn lines(text: &str, allow_blank: bool) -> Result<Vec<&str>, Malformed> {
    let body = text
        .strip_suffix('\n')
        .ok_or(Malformed("output does not end with a newline"))?;
    if body.chars().any(|c| c.is_control() && c != '\n') {
        return Err(Malformed("control characters in output"));
    }
    let lines: Vec<&str> = body.split('\n').collect();
    if lines.len() > MAX_LINES {
        return Err(Malformed("too many lines"));
    }
    if lines.iter().any(|l| l.len() > MAX_LINE_BYTES) {
        return Err(Malformed("line too long"));
    }
    if !allow_blank && lines.iter().any(|l| l.is_empty()) {
        return Err(Malformed("unexpected blank line"));
    }
    Ok(lines)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Scalar(String),
    Dict(Dict),
    Array(Vec<Node>),
}

/// Dictionary entries in output order. Keys are unique.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dict(Vec<(String, Node)>);

impl Dict {
    pub fn get(&self, key: &str) -> Option<&Node> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn entries(&self) -> &[(String, Node)] {
        &self.0
    }
}

/// Parses the entries of a dictionary whose opening line (`... <dictionary> {`)
/// was already consumed, through its closing brace. `depth` is the nesting
/// level of the entries: 1 for a top-level dictionary.
pub fn parse_dict<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
    depth: usize,
) -> Result<Dict, Malformed> {
    let mut entries: Vec<(String, Node)> = Vec::new();
    parse_entries(lines, depth, |key, node| {
        if entries.iter().any(|(k, _)| k == key) {
            return Err(Malformed("duplicate key"));
        }
        entries.push((key.to_owned(), node));
        Ok(())
    })?;
    Ok(Dict(entries))
}

fn parse_array<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
    depth: usize,
) -> Result<Vec<Node>, Malformed> {
    let mut items = Vec::new();
    parse_entries(lines, depth, |key, node| {
        if key != items.len().to_string() {
            return Err(Malformed("array index out of order"));
        }
        items.push(node);
        Ok(())
    })?;
    Ok(items)
}

fn parse_entries<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
    depth: usize,
    mut add: impl FnMut(&str, Node) -> Result<(), Malformed>,
) -> Result<(), Malformed> {
    if depth == 0 || depth > MAX_DEPTH {
        return Err(Malformed("nested too deeply"));
    }
    let mut count = 0;
    loop {
        let line = lines.next().ok_or(Malformed("unterminated container"))?;
        let indent = line.len() - line.trim_start_matches(' ').len();
        let rest = &line[indent..];
        if rest == "}" {
            return if indent == (depth - 1) * 2 {
                Ok(())
            } else {
                Err(Malformed("misaligned closing brace"))
            };
        }
        if indent != depth * 2 {
            return Err(Malformed("misaligned entry"));
        }
        count += 1;
        if count > MAX_ENTRIES {
            return Err(Malformed("too many entries"));
        }
        let (key, value) = split_entry(rest)?;
        let node = match value {
            "<dictionary> {" => Node::Dict(parse_dict(lines, depth + 1)?),
            "<array> {" => Node::Array(parse_array(lines, depth + 1)?),
            v if v.ends_with('{') || v.ends_with('}') => {
                return Err(Malformed("unknown container"));
            }
            v => Node::Scalar(v.to_owned()),
        };
        add(key, node)?;
    }
}

fn split_entry(rest: &str) -> Result<(&str, &str), Malformed> {
    let (key, value) = match rest.split_once(" : ") {
        Some(pair) => pair,
        None => (
            rest.strip_suffix(" :")
                .ok_or(Malformed("entry is not 'key : value'"))?,
            "",
        ),
    };
    let valid_key = !key.is_empty()
        && key.len() <= MAX_KEY_BYTES
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-');
    if valid_key {
        Ok((key, value))
    } else {
        Err(Malformed("invalid key"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Dict, Malformed> {
        let lines = lines(text, false)?;
        let mut iter = lines.into_iter();
        assert_eq!(iter.next(), Some("<dictionary> {"));
        let dict = parse_dict(&mut iter, 1)?;
        assert_eq!(iter.next(), None);
        Ok(dict)
    }

    #[test]
    fn parses_nested_dictionaries_and_arrays() {
        let dict = parse(
            "<dictionary> {\n  A : <array> {\n    0 : x\n    1 : y z\n  }\n  B : \n  C : <dictionary> {\n  }\n}\n",
        )
        .unwrap();
        let Some(Node::Array(items)) = dict.get("A") else {
            panic!("A is not an array: {dict:?}");
        };
        assert_eq!(
            items,
            &[Node::Scalar("x".into()), Node::Scalar("y z".into())]
        );
        assert_eq!(dict.get("B"), Some(&Node::Scalar(String::new())));
        assert_eq!(dict.get("C"), Some(&Node::Dict(Dict::default())));
    }

    #[test]
    fn rejects_malformed_structure() {
        let cases = [
            ("<dictionary> {\n  A : 1\n  A : 2\n}\n", "duplicate key"),
            (
                "<dictionary> {\n  A : <array> {\n    1 : x\n  }\n}\n",
                "array index out of order",
            ),
            ("<dictionary> {\n   A : 1\n}\n", "misaligned entry"),
            ("<dictionary> {\n  A : 1\n  }\n", "misaligned closing brace"),
            ("<dictionary> {\n  A : 1\n", "unterminated container"),
            (
                "<dictionary> {\n  A : <set> {\n  }\n}\n",
                "unknown container",
            ),
            ("<dictionary> {\n  A B : 1\n}\n", "invalid key"),
            ("<dictionary> {\n  A=1\n}\n", "entry is not 'key : value'"),
            (
                "<dictionary> {\n  A : 1\r\n}\n",
                "control characters in output",
            ),
            (
                "<dictionary> {\n  A : 1\n}",
                "output does not end with a newline",
            ),
            ("<dictionary> {\n\n  A : 1\n}\n", "unexpected blank line"),
        ];
        for (text, reason) in cases {
            assert_eq!(parse(text), Err(Malformed(reason)), "{text:?}");
        }
    }

    #[test]
    fn caps_depth_entries_lines_and_line_length() {
        let mut deep = String::from("<dictionary> {\n");
        for level in 1..=MAX_DEPTH {
            deep.push_str(&format!("{}K : <dictionary> {{\n", "  ".repeat(level)));
        }
        for level in (0..=MAX_DEPTH).rev() {
            deep.push_str(&format!("{}}}\n", "  ".repeat(level)));
        }
        assert_eq!(parse(&deep), Err(Malformed("nested too deeply")));

        let mut wide = String::from("<dictionary> {\n");
        for i in 0..=MAX_ENTRIES {
            wide.push_str(&format!("  K{i} : v\n"));
        }
        wide.push_str("}\n");
        assert_eq!(parse(&wide), Err(Malformed("too many entries")));

        let long = format!(
            "<dictionary> {{\n  K : {}\n}}\n",
            "v".repeat(MAX_LINE_BYTES)
        );
        assert_eq!(parse(&long), Err(Malformed("line too long")));

        let many = "x\n".repeat(MAX_LINES + 1);
        assert_eq!(lines(&many, false), Err(Malformed("too many lines")));
    }

    #[test]
    fn accepts_nesting_up_to_the_limit() {
        let mut deep = String::from("<dictionary> {\n");
        for level in 1..MAX_DEPTH {
            deep.push_str(&format!("{}K : <dictionary> {{\n", "  ".repeat(level)));
        }
        for level in (0..MAX_DEPTH).rev() {
            deep.push_str(&format!("{}}}\n", "  ".repeat(level)));
        }
        assert!(parse(&deep).is_ok());
    }
}
