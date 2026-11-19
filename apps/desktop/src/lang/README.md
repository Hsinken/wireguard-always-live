# Language files

Every word the WG Always Live window, its tray menu and its Telegram
messages show comes from the files in this folder, one per language. The
code has no user-visible text of its own.

| File | Language | Stands for system languages |
|---|---|---|
| `zh-TW` | 繁體中文 (Traditional Chinese) | `zh-TW`, `zh-HK`, `zh-MO`, `zh-Hant` |
| `en` | English (the fallback) | `en` |
| `zh-CN` | 简体中文 (Simplified Chinese) | `zh`, `zh-CN`, `zh-SG`, `zh-Hans` |
| `ja` | 日本語 (Japanese) | `ja` |
| `ko` | 한국어 (Korean) | `ko` |
| `de` | Deutsch (German) | `de` |
| `es` | Español (Spanish) | `es` |
| `pt-BR` | Português (Brasil) (Brazilian Portuguese) | `pt` |
| `tr` | Türkçe (Turkish) | `tr` |
| `fr` | Français (French) | `fr` |
| `ru` | Русский (Russian) | `ru` |
| `id` | Bahasa Indonesia (Indonesian) | `id`, `in` |
| `vi` | Tiếng Việt (Vietnamese) | `vi` |

## How the window picks a language

By default it follows the system: it takes the system's preferred
languages in order and uses the first one a language here stands for,
by the longest matching prefix in `languages.json` (`zh-Hant-HK` is
Traditional Chinese through `zh-Hant`, plain `zh` is Simplified Chinese,
`pt-PT` is Brazilian Portuguese). When none matches it speaks English.
The user can also pick a language under Settings; the choice is kept on
that computer.

A key a file lacks falls back to English, but the tests require every
file to have every key, so that never shows in a release.

## Adding a language

1. Copy `en.json` to `<tag>.json`, where `<tag>` is the BCP 47 tag: a
   language, then optionally a script and a region (`it`, `pt-PT`,
   `zh-Hant-HK`). Set `_meta.tag` to the same tag and `_meta.name` to the
   language's own name for itself (`Italiano`).
2. Translate every value. Keep the keys, and keep every `{placeholder}`
   exactly as it is (the code fills it in); `{{` and `}}` stand for
   literal braces. Button texts and tray menu items never end in `…`.
3. `notification_templates` are the Telegram messages. The watchdog
   refuses a set unless each template uses each of its placeholders
   exactly once, and no other, within 1,024 bytes (see
   `crates/wgal-protocol/src/template.rs`). Keep the line structure of
   the English ones.
4. Add one line to `languages.json`, where you want the language in the
   picker: its tag, and the system language prefixes it stands for (each
   prefix may belong to one language only).
5. Add it to the table above, then run `pnpm run check` in `apps/desktop`
   and `cargo test -p wgal-desktop`; the tests check the keys,
   placeholders, templates, the list and this table.

No code changes: the window and the tray read every listed file at build
time.

## Notes for translators

- "WG Always Live", "WGAL", "WireGuard", "WatchDog", "MCP", "Telegram",
  "Bot Token" and "Chat ID" are names; keep them as they are.
- The watchdog is WGAL's background service; the interface is this window.
- Address examples use documentation addresses (`192.0.2.1`) and must stay
  that way.
