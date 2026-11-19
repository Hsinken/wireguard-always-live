//! `wgal`'s own words, in the language of this run (owner decisions of
//! 2026-11-19): every sentence it writes is looked up by key in one of the
//! 13 files in `lang/`, one per language of the window, with the window's
//! tags. `en.json` is the source: it has every key the program uses, and a
//! key another file lacks is written in English, key by key, as the window
//! falls back. The files are built into the program; nothing is read from
//! the disk or the environment.
//!
//! A text names its values as `{name}`, and `{{` and `}}` stand for the
//! braces themselves. Every language's text for a key names the same
//! values as the English one, no more and no fewer (the tests check).
//!
//! The language of a run is set once, when it is known (the plan's §3.1),
//! for the thread that runs it; [`tr!`] then writes in it. A run starts in
//! English.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt::{Display, Write as _};
use std::sync::LazyLock;

/// The files, in the order of `languages.json` (the window's order).
pub const FILES: &[(&str, &str)] = &[
    ("zh-TW", include_str!("lang/zh-TW.json")),
    ("en", include_str!("lang/en.json")),
    ("zh-CN", include_str!("lang/zh-CN.json")),
    ("ja", include_str!("lang/ja.json")),
    ("ko", include_str!("lang/ko.json")),
    ("de", include_str!("lang/de.json")),
    ("es", include_str!("lang/es.json")),
    ("pt-BR", include_str!("lang/pt-BR.json")),
    ("tr", include_str!("lang/tr.json")),
    ("fr", include_str!("lang/fr.json")),
    ("ru", include_str!("lang/ru.json")),
    ("id", include_str!("lang/id.json")),
    ("vi", include_str!("lang/vi.json")),
];

/// English's place in [`FILES`].
const ENGLISH: usize = 1;

type Catalog = BTreeMap<String, String>;

/// Every file, read once. A file that does not read (the tests make sure
/// none) counts as empty, so its language is all English.
fn catalogs() -> &'static [Catalog] {
    static ALL: LazyLock<Vec<Catalog>> = LazyLock::new(|| {
        FILES
            .iter()
            .map(|(_, json)| serde_json::from_str(json).unwrap_or_default())
            .collect()
    });
    &ALL
}

thread_local! {
    static CURRENT: Cell<usize> = const { Cell::new(ENGLISH) };
}

/// Writes in the language tagged `tag` (one of [`FILES`], in any case)
/// from now on, on this thread; English for any other tag. Returns the tag
/// as [`FILES`] spells it.
pub fn set(tag: &str) -> &'static str {
    let index = FILES
        .iter()
        .position(|(t, _)| t.eq_ignore_ascii_case(tag))
        .unwrap_or(ENGLISH);
    CURRENT.with(|c| c.set(index));
    FILES[index].0
}

/// Writes in English from now on, on this thread.
pub fn english() {
    CURRENT.with(|c| c.set(ENGLISH));
}

/// The tag of the language written in now.
#[cfg(test)]
pub fn current() -> &'static str {
    FILES[CURRENT.with(Cell::get)].0
}

/// Whether `tag` is one of the 13 languages, in any case; the spelling of
/// [`FILES`] when it is.
pub fn known(tag: &str) -> Option<&'static str> {
    FILES
        .iter()
        .find(|(t, _)| t.eq_ignore_ascii_case(tag))
        .map(|(t, _)| *t)
}

/// The text for `key` in the language of now, or in English when that
/// language has none, with its values filled in.
pub fn render(key: &str, values: &[(&str, &dyn Display)]) -> String {
    let all = catalogs();
    let text = all[CURRENT.with(Cell::get)]
        .get(key)
        .or_else(|| all[ENGLISH].get(key))
        .map_or(key, String::as_str);
    fill(text, values)
}

/// `text` with each `{name}` replaced by its value and `{{`, `}}` by one
/// brace. A name with no value is left as it is written (the tests make
/// sure there is none).
pub fn fill(text: &str, values: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(['{', '}']) {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        if let Some(after) = tail.strip_prefix("{{") {
            out.push('{');
            rest = after;
        } else if let Some(after) = tail.strip_prefix("}}") {
            out.push('}');
            rest = after;
        } else if let Some(name) = tail
            .strip_prefix('{')
            .and_then(|t| t.split_once('}'))
            .map(|(name, _)| name)
            .filter(|name| is_name(name))
        {
            match values.iter().find(|(n, _)| *n == name) {
                Some((_, value)) => {
                    let _ = write!(out, "{value}");
                }
                None => {
                    out.push('{');
                    out.push_str(name);
                    out.push('}');
                }
            }
            rest = &tail[name.len() + 2..];
        } else {
            out.push_str(&tail[..1]);
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

fn is_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// The names `text` uses, each once.
#[cfg(test)]
pub fn names(text: &str) -> std::collections::BTreeSet<String> {
    let mut found = std::collections::BTreeSet::new();
    let mut rest = text;
    while let Some(at) = rest.find(['{', '}']) {
        let tail = &rest[at..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            rest = &tail[2..];
        } else if let Some(name) = tail
            .strip_prefix('{')
            .and_then(|t| t.split_once('}'))
            .map(|(name, _)| name)
            .filter(|name| is_name(name))
        {
            found.insert(name.to_owned());
            rest = &tail[name.len() + 2..];
        } else {
            rest = &tail[1..];
        }
    }
    found
}

/// How many columns `text` takes in a terminal: East Asian wide and full
/// width characters take two, the rest one. The tests hold the help
/// texts of every language to a terminal's 80 columns with it.
#[cfg(test)]
pub fn width(text: &str) -> usize {
    text.chars().map(|c| if wide(c) { 2 } else { 1 }).sum()
}

#[cfg(test)]
fn wide(c: char) -> bool {
    matches!(u32::from(c),
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x2FFFD
        | 0x30000..=0x3FFFD)
}

/// A sentence of `wgal`'s in the language of this run: `tr!("key")`, or
/// `tr!("key", name = value, ...)` for a text with values.
macro_rules! tr {
    ($key:literal) => {
        $crate::text::render($key, &[])
    };
    ($key:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::text::render(
            $key,
            &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),+],
        )
    };
}
pub(crate) use tr;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn values_are_filled_and_braces_doubled_stand_for_one() {
        assert_eq!(
            fill("a {x} b {y}", &[("x", &1), ("y", &"two")]),
            "a 1 b two"
        );
        assert_eq!(fill("{{x}} {x}", &[("x", &3)]), "{x} 3");
        assert_eq!(fill("{x} {x}", &[("x", &"a")]), "a a");
        assert_eq!(fill("{missing}", &[]), "{missing}");
        assert_eq!(fill("{ not a name } {", &[]), "{ not a name } {");
        assert_eq!(fill("中文 {x}。", &[("x", &"ok")]), "中文 ok。");
        assert_eq!(
            names("{a} {{b}} {c_2} {a} { d }"),
            ["a", "c_2"].iter().map(|s| (*s).to_owned()).collect()
        );
    }

    #[test]
    fn wide_characters_take_two_columns() {
        assert_eq!(width("status"), 6);
        assert_eq!(width("隧道"), 4);
        assert_eq!(width("トンネル"), 8);
        assert_eq!(width("터널"), 4);
        assert_eq!(width("Ünïcode"), 7);
    }

    #[test]
    fn the_language_is_set_per_thread_and_unknown_tags_are_english() {
        english();
        assert_eq!(current(), "en");
        assert_eq!(set("ZH-tw"), "zh-TW");
        assert_eq!(current(), "zh-TW");
        std::thread::spawn(|| assert_eq!(current(), "en"))
            .join()
            .unwrap();
        assert_eq!(set("xx"), "en");
        assert_eq!(known("pt-br"), Some("pt-BR"));
        assert_eq!(known("pt"), None);
        english();
    }

    #[test]
    fn the_files_are_the_window_s_languages_in_its_order() {
        let tags: Vec<&str> = crate::languages::all()
            .iter()
            .map(|l| l.tag.as_str())
            .collect();
        let files: Vec<&str> = FILES.iter().map(|(t, _)| *t).collect();
        assert_eq!(files, tags);
        assert_eq!(FILES[ENGLISH].0, "en");
    }

    #[test]
    fn every_file_reads_and_says_only_what_english_says() {
        let english: Catalog = serde_json::from_str(FILES[ENGLISH].1).unwrap();
        assert!(!english.is_empty());
        for (tag, json) in FILES {
            let catalog: Catalog =
                serde_json::from_str(json).unwrap_or_else(|e| panic!("lang/{tag}.json: {e}"));
            for (key, text) in &catalog {
                let source = english.get(key).unwrap_or_else(|| {
                    panic!("lang/{tag}.json has {key:?}, which en.json has not")
                });
                assert_eq!(
                    names(text),
                    names(source),
                    "lang/{tag}.json {key:?} names other values than English"
                );
                assert!(
                    !text.chars().any(|c| c.is_control() && c != '\n'),
                    "lang/{tag}.json {key:?} has a control character"
                );
                assert_eq!(
                    wgal_client::token::without_tokens(text),
                    *text,
                    "lang/{tag}.json {key:?} holds text shaped like a Bot Token"
                );
                assert!(!text.is_empty(), "lang/{tag}.json {key:?} is empty");
            }
        }
    }

    /// Every language says everything English says: a key left out would
    /// be written in English in the middle of another language.
    #[test]
    fn every_language_has_every_key() {
        let english: Catalog = serde_json::from_str(FILES[ENGLISH].1).unwrap();
        for (tag, json) in FILES {
            let catalog: Catalog = serde_json::from_str(json).unwrap();
            let missing: Vec<&String> = english
                .keys()
                .filter(|k| !catalog.contains_key(*k))
                .collect();
            assert!(missing.is_empty(), "lang/{tag}.json lacks {missing:?}");
        }
    }

    /// Every line of the help fits a terminal of 80 columns in every
    /// language, English included, East Asian characters taking two (the
    /// plan's §3.3).
    #[test]
    fn help_lines_fit_80_columns() {
        for (tag, json) in FILES {
            let catalog: Catalog = serde_json::from_str(json).unwrap();
            for (key, text) in catalog.iter().filter(|(k, _)| k.starts_with("help.")) {
                for line in text.lines() {
                    assert!(
                        width(line) <= 80,
                        "lang/{tag}.json {key:?}: {} columns: {line}",
                        width(line)
                    );
                }
            }
        }
    }

    /// The keys the program uses, as `tr!("key"` in its sources (rustfmt
    /// may put the key on the next line).
    fn used_keys() -> std::collections::BTreeSet<String> {
        let sources = [
            include_str!("main.rs"),
            include_str!("args.rs"),
            include_str!("console.rs"),
            include_str!("languages.rs"),
            include_str!("run.rs"),
            include_str!("show.rs"),
        ];
        let mut keys = std::collections::BTreeSet::new();
        for source in sources {
            let pieces: Vec<&str> = source.split("tr!(").collect();
            for pair in pieces.windows(2) {
                let [before, piece] = pair else { continue };
                // `include_str!(` ends in `tr!(` too: only the macro
                // itself counts.
                if before.ends_with(|c: char| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                if let Some((key, _)) = piece
                    .trim_start()
                    .strip_prefix('"')
                    .and_then(|p| p.split_once('"'))
                {
                    keys.insert(key.to_owned());
                }
            }
        }
        keys
    }

    #[test]
    fn english_has_every_key_the_program_uses_and_no_other() {
        let english: Catalog = serde_json::from_str(FILES[ENGLISH].1).unwrap();
        let used = used_keys();
        let missing: Vec<&String> = used.iter().filter(|k| !english.contains_key(*k)).collect();
        assert!(missing.is_empty(), "en.json lacks {missing:?}");
        let unused: Vec<&String> = english.keys().filter(|k| !used.contains(*k)).collect();
        assert!(
            unused.is_empty(),
            "en.json has keys no code uses: {unused:?}"
        );
    }
}
