# Languages

WG Always Live speaks 13 languages: 繁體中文, English, 简体中文, 日本語,
한국어, Deutsch, Español, Português (Brasil), Türkçe, Français, Русский,
Bahasa Indonesia and Tiếng Việt. English is the default and the fallback.
Translations other than Traditional Chinese and English are first drafts:
corrections are welcome.

Three things are translated, each in its own place:

| What | Where | Source language |
|---|---|---|
| The window, its tray or menu bar menu | `apps/desktop/src/lang/<tag>.json`, listed in `apps/desktop/src/lang/languages.json` | `en.json` |
| The Telegram messages | `notification_templates` inside the same window files | the English templates |
| The command line, `wgal` | `crates/wgal-cli/src/lang/<tag>.json` | `en.json` |

The watchdog itself never writes a sentence for the user: it reports codes,
reasons and states, and writes Telegram messages only from the templates it
was given, or from its built-in English set.

## Adding a language

The window's language list is the list: the command line has one file for
each of its languages, with the same tags, in the same order.

1. **The window**: follow [apps/desktop/src/lang/README.md](../apps/desktop/src/lang/README.md).
   In short:
   - copy `en.json` to `<tag>.json` (a BCP 47 tag such as `it` or
     `pt-PT`), and set `_meta.tag` and `_meta.name`;
   - translate every value, keeping every key and every `{placeholder}`;
   - add one line to `languages.json` and one row to the README's table.
2. **The Telegram messages** are that file's `notification_templates`.
   The watchdog refuses a set unless each template uses each of its
   placeholders exactly once, and no other, within 1,024 bytes
   (`crates/wgal-protocol/src/template.rs`). Keep the line structure of the
   English ones.
3. **The command line**:
   - copy `crates/wgal-cli/src/lang/en.json` to `<tag>.json` and translate
     every value. Each text must name the same `{values}` as the English
     one, no more and no fewer;
   - add the file to the list in `crates/wgal-cli/src/text.rs`, in the
     window's order.
4. **Regenerate the command line's copy of the Telegram sets**:
   `WGAL_UPDATE_FIXTURES=1 cargo test -p wgal-cli`. It writes
   `crates/wgal-cli/src/languages.json` from the window's files, so that
   `wgal notify language <tag>` can hand the set over.
5. **Run the checks**: `pnpm run check`, `cargo test -p wgal-desktop` and
   `cargo test -p wgal-cli`. The tests check, for every language:
   - the keys and the placeholders;
   - the template rules;
   - the list and its order, and that the command line's files match the
     window's languages.

A new language needs no change to the shared contract. A new placeholder,
template or reason does: see "Changing the shared contract" in
[AGENTS.md](../AGENTS.md).

## Changing a text

- **The window**: edit the key in every file that should change.
- **A Telegram template**: edit it in the window's file, then run step 4 so
  `wgal` hands over the same text.
- **The command line**: edit `crates/wgal-cli/src/lang/<tag>.json`.

A key a file lacks falls back to English, key by key, but the tests require
every file to have every key.

## How each part picks its language

| Part | Picks |
|---|---|
| The window | The system's language, unless the user picks one under Settings |
| `wgal` | `--lang <tag>` for one command; otherwise the shared language set with `wgal language <tag>`; otherwise English |
| Telegram messages | The set the window or `wgal` handed over (`wgal language <tag>` or `wgal notify language <tag>`); otherwise the built-in English set |

- The window hands over its own language's set only while the watchdog
  still uses the built-in English texts. It never changes `wgal`'s
  language.
- A language chosen with `wgal` stays until someone chooses another.

## Notes for translators

- **Names stay as they are**: "WG Always Live", "WGAL", "WireGuard",
  "WatchDog", "MCP", "Telegram", "Bot Token" and "Chat ID".
- **Two words to tell apart**: the watchdog is WGAL's background service;
  the interface is the window.
- **Address examples** use documentation addresses (`192.0.2.1`) and must
  stay that way.
