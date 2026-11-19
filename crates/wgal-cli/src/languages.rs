//! The languages Telegram messages can be written in (owner decision of
//! 2026-11-18: `wgal notify language <tag>` makes the watchdog write them
//! in that language; with none set it writes English, its built-in set).
//! `languages.json` holds, for each of the window's 13 languages in the
//! window's order, its tag, its name and the ten texts of its
//! `notification_templates`, nothing else of the window's words. A test
//! writes it from the window's language files (`apps/desktop/src/lang/`)
//! and fails when they differ.
//!
//! Each set is named as the window names it (`templatesId` in
//! `apps/desktop/src/templates.ts`): its tag, `/`, and the FNV-1a 32 hash
//! of its texts. The same texts get the same name here and in any window,
//! so a window in the same language sees the set `wgal` handed over as its
//! own, and one in another language leaves it alone.

use std::sync::LazyLock;

use serde::Deserialize;
use wgal_protocol::{NotificationTemplates, ProductVersion, TemplatesId};

use crate::text::tr;

/// One language and its set.
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Language {
    pub tag: String,
    pub name: String,
    pub templates: NotificationTemplates,
}

/// The table, as the test writes it.
pub const TABLE: &str = include_str!("languages.json");

/// Every language, in the window's order.
pub fn all() -> &'static [Language] {
    static ALL: LazyLock<Vec<Language>> = LazyLock::new(|| {
        // The tests read this very table and check every set.
        serde_json::from_str(TABLE).expect("languages.json is a valid table")
    });
    &ALL
}

/// The language whose tag is `tag`, in any case.
pub fn find(tag: &str) -> Option<&'static Language> {
    all().iter().find(|l| l.tag.eq_ignore_ascii_case(tag))
}

/// FNV-1a, 32 bits, over `bytes`, as the window computes it.
#[cfg(test)]
pub fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for &byte in bytes {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// What the window joins the texts with for the hash: a line break and a
/// NUL, which no template may hold.
#[cfg(test)]
pub const HASH_SEPARATOR: &str = "\n\u{0}";

/// The name the window gives a set of `texts`, in the contract's order, in
/// language `tag`.
#[cfg(test)]
pub fn templates_id(tag: &str, texts: &[&str]) -> String {
    format!(
        "{tag}/{:08x}",
        fnv1a32(texts.join(HASH_SEPARATOR).as_bytes())
    )
}

/// What a set's name says before its first `/`.
fn tag_of(id: &str) -> &str {
    id.split_once('/').map_or(id, |(tag, _)| tag)
}

/// Which set the watchdog writes with.
#[derive(Debug, PartialEq, Eq)]
pub enum InUse {
    /// Its own built-in set: English from watchdog 3.0.0, Traditional
    /// Chinese before.
    BuiltIn { english: bool },
    /// One of this table's.
    Known(&'static Language),
    /// A set in one of this table's languages with other texts (another
    /// version of the window handed it over).
    OtherTexts(&'static Language),
    /// A set this table does not know.
    Unknown(String),
}

/// From which watchdog version the built-in set is English.
pub const ENGLISH_BUILT_IN_SINCE: &str = "3.0.0";

/// Which set a watchdog of `version` that names `id` in its snapshot
/// writes with.
pub fn in_use(id: Option<&TemplatesId>, version: &ProductVersion) -> InUse {
    let Some(id) = id else {
        let english = ProductVersion::new(ENGLISH_BUILT_IN_SINCE)
            .is_ok_and(|since| version.is_at_least(&since));
        return InUse::BuiltIn { english };
    };
    let id = id.as_str();
    if let Some(known) = all().iter().find(|l| l.templates.id().as_str() == id) {
        return InUse::Known(known);
    }
    match find(tag_of(id)) {
        Some(language) => InUse::OtherTexts(language),
        None => InUse::Unknown(id.to_owned()),
    }
}

impl InUse {
    /// In words.
    pub fn describe(&self) -> String {
        match self {
            Self::BuiltIn { english: true } => tr!("language.built_in_english"),
            Self::BuiltIn { english: false } => tr!("language.built_in_chinese"),
            Self::Known(l) => tr!("language.known", language = l.name, tag = l.tag),
            Self::OtherTexts(l) => tr!("language.other_texts", language = l.name, tag = l.tag),
            Self::Unknown(id) => tr!("language.unknown", id = id),
        }
    }

    /// The tag, when it is one of the table's languages.
    pub fn tag(&self) -> Option<&'static str> {
        match self {
            Self::BuiltIn { english: true } => Some("en"),
            Self::BuiltIn { english: false } => Some("zh-TW"),
            Self::Known(l) | Self::OtherTexts(l) => Some(l.tag.as_str()),
            Self::Unknown(_) => None,
        }
    }
}
