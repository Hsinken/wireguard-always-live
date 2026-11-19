//! The tray's words, from the GUI's own language files (`src/lang/*.json`,
//! owner's requests of 2026-11-03 and 2026-11-05), which `build.rs`
//! compiles in with `include_str!`, so a new file needs no code change here
//! either. The window tells the shell which language it shows by its tag in
//! every `update_tray`; the shell looks the tag up in its compiled list, and
//! an unknown one gets English, the fallback every file falls back to key by
//! key. No text from the webview is ever shown: only the tag crosses, and
//! the texts are this binary's own.
//!
//! For the window's "follow the system" choice the shell also says which
//! languages the system prefers (`system_languages`), read with
//! `sys-locale` and reduced to plain tags; the window picks the language.
//!
//! Only `_meta` and `tray` are read from a file, with serde_json; the
//! window's sections are left alone.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

/// The tag of the language every other one falls back to.
pub(crate) const FALLBACK_TAG: &str = "en";

/// The compiled language files: (tag, content), from the file names.
const FILES: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/languages.rs"));

/// One text the tray uses, by its key under `tray.` in a language file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Text {
    MenuOpen,
    MenuPause,
    MenuQuit,
    MenuStop,
    MonitorOk,
    MonitorPaused,
    MonitorLost,
    MonitorStorageFailed,
    LevelLost,
    LevelFault,
    LevelExpired,
    LevelHost,
    LevelPaused,
    LevelAttention,
    LevelOk,
    TooltipLost,
    Tooltip,
    TooltipStale,
}

impl Text {
    pub(crate) const ALL: [Self; 18] = [
        Self::MenuOpen,
        Self::MenuPause,
        Self::MenuQuit,
        Self::MenuStop,
        Self::MonitorOk,
        Self::MonitorPaused,
        Self::MonitorLost,
        Self::MonitorStorageFailed,
        Self::LevelLost,
        Self::LevelFault,
        Self::LevelExpired,
        Self::LevelHost,
        Self::LevelPaused,
        Self::LevelAttention,
        Self::LevelOk,
        Self::TooltipLost,
        Self::Tooltip,
        Self::TooltipStale,
    ];

    /// The key under `tray.` in the language files.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::MenuOpen => "menu_open",
            Self::MenuPause => "menu_pause",
            Self::MenuQuit => "menu_quit",
            Self::MenuStop => "menu_stop",
            Self::MonitorOk => "monitor_ok",
            Self::MonitorPaused => "monitor_paused",
            Self::MonitorLost => "monitor_lost",
            Self::MonitorStorageFailed => "monitor_storage_failed",
            Self::LevelLost => "level_lost",
            Self::LevelFault => "level_fault",
            Self::LevelExpired => "level_expired",
            Self::LevelHost => "level_host",
            Self::LevelPaused => "level_paused",
            Self::LevelAttention => "level_attention",
            Self::LevelOk => "level_ok",
            Self::TooltipLost => "tooltip_lost",
            Self::Tooltip => "tooltip",
            Self::TooltipStale => "tooltip_stale",
        }
    }
}

/// One language as the tray needs it.
#[derive(Debug)]
pub(crate) struct Language {
    pub(crate) tag: String,
    texts: Vec<String>,
}

impl Language {
    pub(crate) fn text(&self, text: Text) -> &str {
        self.texts.get(text as usize).map_or("", String::as_str)
    }
}

/// What the shell reads of a language file.
#[derive(Debug, Deserialize)]
struct File {
    #[serde(rename = "_meta")]
    meta: Meta,
    #[serde(default)]
    tray: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct Meta {
    tag: String,
}

/// A language file's `_meta` and `tray`, or `None` when it is not valid
/// JSON of that shape (a `tray` text that is not a string included).
fn read(content: &str) -> Option<File> {
    serde_json::from_str(content).ok()
}

/// Every compiled language, the fallback first. A file that cannot be
/// read, or whose `_meta.tag` is not its name, is left out; a key a file
/// lacks takes the fallback's text, and one the fallback lacks too is its
/// own key (the tests make sure neither happens).
pub(crate) fn languages() -> &'static [Language] {
    static ALL: OnceLock<Vec<Language>> = OnceLock::new();
    ALL.get_or_init(|| {
        let fallback = FILES
            .iter()
            .find(|(tag, _)| *tag == FALLBACK_TAG)
            .and_then(|(_, content)| read(content))
            .map(|file| file.tray)
            .unwrap_or_default();
        let lookup = |own: &BTreeMap<String, String>, text: Text| {
            own.get(text.key())
                .or_else(|| fallback.get(text.key()))
                .cloned()
                .unwrap_or_else(|| format!("tray.{}", text.key()))
        };
        let mut all: Vec<Language> = FILES
            .iter()
            .filter_map(|(tag, content)| {
                // As in the window: a file must name itself by its own tag.
                let file = read(content).filter(|file| file.meta.tag == *tag)?;
                Some(Language {
                    tag: (*tag).to_owned(),
                    texts: Text::ALL
                        .iter()
                        .map(|&text| lookup(&file.tray, text))
                        .collect(),
                })
            })
            .collect();
        if !all.iter().any(|l| l.tag == FALLBACK_TAG) {
            all.push(Language {
                tag: FALLBACK_TAG.to_owned(),
                texts: Text::ALL
                    .iter()
                    .map(|&text| lookup(&BTreeMap::new(), text))
                    .collect(),
            });
        }
        all.sort_by_key(|l| l.tag != FALLBACK_TAG);
        all
    })
}

/// The language a tag names, or the fallback for a tag with no file.
pub(crate) fn find(tag: &str) -> &'static Language {
    let all = languages();
    all.iter()
        .find(|l| l.tag == tag)
        .or_else(|| all.iter().find(|l| l.tag == FALLBACK_TAG))
        .unwrap_or(&all[0])
}

/// Most system languages the shell hands the window.
pub(crate) const MAX_SYSTEM_LANGUAGES: usize = 8;

/// The system's languages as the window takes them, most preferred first:
/// each reduced to a plain BCP 47 tag (`zh_TW.UTF-8` is `zh-TW`), anything
/// that is not one left out, each listed once, at most
/// `MAX_SYSTEM_LANGUAGES`. Only the first 64 the system gives are looked at.
pub(crate) fn system_languages(found: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for locale in found.into_iter().take(64) {
        let Some(tag) = system_tag(&locale) else {
            continue;
        };
        if !out.contains(&tag) {
            out.push(tag);
        }
        if out.len() == MAX_SYSTEM_LANGUAGES {
            break;
        }
    }
    out
}

/// A system locale as a tag: without a POSIX code set or modifier, with
/// `-` for `_`; a language of 2 or 3 letters, then subtags of 1 to 8
/// letters and digits, 35 bytes at most. `None` for anything else.
fn system_tag(locale: &str) -> Option<String> {
    let base = locale.split(['.', '@']).next()?;
    let tag = base.replace('_', "-");
    let valid = tag.len() <= 35
        && tag.split('-').enumerate().all(|(i, part)| {
            if i == 0 {
                (2..=3).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_alphabetic())
            } else {
                (1..=8).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_alphanumeric())
            }
        });
    valid.then_some(tag)
}

/// A placeholder's name: 1 to 24 of `a`–`z` and `_`.
fn is_name(name: &str) -> bool {
    (1..=24).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// `template` with each `{name}` replaced by its value in `values`; `{{`
/// and `}}` are literal braces, the form of the language files and the
/// contract's message templates. A name with no value stays as written.
pub(crate) fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(at) = rest.find(['{', '}']) {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        if let Some(after) = tail.strip_prefix("{{") {
            out.push('{');
            rest = after;
        } else if let Some(after) = tail.strip_prefix("}}") {
            out.push('}');
            rest = after;
        } else if let Some((name, after)) = tail
            .strip_prefix('{')
            .and_then(|body| body.split_once('}'))
            .filter(|(name, _)| is_name(name))
        {
            match values.iter().find(|(n, _)| *n == name) {
                Some((_, value)) => out.push_str(value),
                None => {
                    out.push('{');
                    out.push_str(name);
                    out.push('}');
                }
            }
            rest = after;
        } else {
            // A brace that opens nothing: kept as it is. Braces are ASCII,
            // so this falls on a character boundary.
            out.push_str(&tail[..1]);
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reference_file_is_compiled_in_and_comes_first() {
        let all = languages();
        assert_eq!(all[0].tag, FALLBACK_TAG);
        assert!(FILES.iter().any(|(tag, _)| *tag == FALLBACK_TAG));
        // Every file is read: none is left out for being broken.
        assert_eq!(all.len(), FILES.len());
        for (tag, content) in FILES {
            let file = read(content).unwrap_or_else(|| panic!("{tag}.json cannot be read"));
            assert_eq!(file.meta.tag, *tag);
        }
    }

    /// The reference has every tray text, and every language uses the same
    /// placeholders in each as the reference, so `fill` fills them all.
    #[test]
    fn every_language_has_every_tray_text_with_its_placeholders() {
        let names = |text: &str| {
            let mut found = Vec::new();
            let mut rest = text;
            while let Some(at) = rest.find('{') {
                let tail = &rest[at + 1..];
                if let Some(end) = tail.find('}') {
                    found.push(tail[..end].to_owned());
                    rest = &tail[end + 1..];
                } else {
                    break;
                }
            }
            found.sort();
            found
        };
        let (_, reference) = FILES
            .iter()
            .find(|(tag, _)| *tag == FALLBACK_TAG)
            .expect("reference");
        let own = read(reference).expect("en.json").tray;
        for text in Text::ALL {
            assert!(own.contains_key(text.key()), "en lacks tray.{}", text.key());
        }
        let fallback = find(FALLBACK_TAG);
        for language in languages() {
            for text in Text::ALL {
                assert_eq!(
                    names(language.text(text)),
                    names(fallback.text(text)),
                    "{} tray.{}",
                    language.tag,
                    text.key()
                );
            }
        }
        assert_eq!(
            names(fallback.text(Text::Tooltip)),
            ["host_issues", "level", "monitor", "vpn_faults"]
        );
    }

    #[test]
    fn an_unknown_tag_gets_english() {
        assert_eq!(find("zh-TW").tag, "zh-TW");
        for tag in ["", "xx-YY", "zh-tw", "../zh-TW", "zh-TW\0"] {
            assert_eq!(find(tag).tag, FALLBACK_TAG, "{tag:?}");
        }
        assert_eq!(find("zh-TW").text(Text::MenuOpen), "開啟視窗");
        assert_eq!(find("xx-YY").text(Text::MenuOpen), "Open window");
    }

    #[test]
    fn system_languages_are_plain_tags_most_preferred_first() {
        let found = |locales: &[&str]| system_languages(locales.iter().map(|l| (*l).to_owned()));
        assert_eq!(
            found(&["zh-Hant-TW", "en-US", "ja"]),
            ["zh-Hant-TW", "en-US", "ja"]
        );
        // POSIX locales lose their code set and modifier; `C`, `POSIX`
        // and anything that is not a tag are left out, and so is a repeat.
        assert_eq!(
            found(&[
                "zh_TW.UTF-8",
                "de_DE@euro",
                "C",
                "POSIX",
                "",
                "zh-TW",
                "x",
                "en-US;q=1",
                "../en"
            ]),
            ["zh-TW", "de-DE"]
        );
        assert_eq!(
            found(&["en-abcdefghi", "a-b", "abcd"]),
            Vec::<String>::new()
        );
        let many: Vec<String> = (0..40).map(|n| format!("en-{n:02}x")).collect();
        assert_eq!(system_languages(many).len(), MAX_SYSTEM_LANGUAGES);
    }

    #[test]
    fn placeholders_are_filled_and_braces_kept() {
        assert_eq!(fill("a {x} b {y}", &[("x", "1"), ("y", "2")]), "a 1 b 2");
        assert_eq!(fill("{{x}} {x}", &[("x", "1")]), "{x} 1");
        assert_eq!(fill("{missing}", &[]), "{missing}");
        assert_eq!(fill("{ x } {", &[("x", "1")]), "{ x } {");
        assert_eq!(fill("}{X}", &[("X", "1")]), "}{X}");
        // Values are inserted as they are, never read again.
        assert_eq!(fill("{x}", &[("x", "{y}")]), "{y}");
        assert_eq!(fill("監看 {n} 條", &[("n", "16")]), "監看 16 條");
    }
}
