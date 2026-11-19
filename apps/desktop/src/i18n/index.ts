// The window's words (owner's requests of 2026-11-03 and 2026-11-05).
// Every sentence the user reads comes from a language file in
// `src/lang/`, one per language, named by its BCP 47 tag (`zh-TW.json`):
// nested sections of strings, with `_meta` naming the language and
// `notification_templates` holding the texts the watchdog writes Telegram
// messages with. `src/lang/languages.json` lists the languages, in the
// order the picker offers them, with the system languages each one
// stands for, and names the fallback. Everything there is compiled in at
// build time, so adding a language is a new file and one line in the
// list, with no code change (`src/lang/README.md`). English is the
// fallback: its keys type `t`, a key another file lacks falls back to
// it, and it is spoken when the system's languages have no file. No DOM
// here; `dom.ts` relabels the page.
//
// A placeholder is `{name}` (`a`–`z`, `_`), the form of the contract's
// message templates; `{{` and `}}` are literal braces.

/// <reference types="vite/client" />

import en from "../lang/en.json";
import listFile from "../lang/languages.json";

/// The language every other one falls back to, key by key, and the one
/// spoken when none of the system's languages has a file.
export const FALLBACK_TAG = "en";

/// The choice that follows the system's language (owner request of
/// 2026-11-05); every other choice is a language's tag.
export const SYSTEM_CHOICE = "system";

type Leaves<T, P extends string = ""> = {
  [K in keyof T & string]: T[K] extends string ? `${P}${K}` : T[K] extends object ? Leaves<T[K], `${P}${K}.`> : never;
}[keyof T & string];

/// Every key of the language files, as a dotted path to its string
/// (`light_reason.verified`), typed from the English file so `tsc`
/// rejects a key it does not have.
export type Key = Leaves<typeof en>;

/// Values for a text's placeholders.
export type Params = Readonly<Record<string, string | number>>;

export interface Language {
  /// The BCP 47 tag, also the file's name.
  readonly tag: string;
  /// The language's own name for itself, for the language picker.
  readonly name: string;
  /// The system languages it stands for, as BCP 47 prefixes (`zh-Hant`
  /// stands for `zh-Hant-TW`), from the list.
  readonly system: readonly string[];
  /// Every string, by its dotted key.
  readonly strings: ReadonlyMap<string, string>;
}

/// One language in `languages.json`.
export interface ListedLanguage {
  readonly tag: string;
  readonly system: readonly string[];
}

/// `languages.json`: the languages in the picker's order, and the fallback.
export interface LanguageList {
  readonly fallback: string;
  readonly languages: readonly ListedLanguage[];
}

/// A language file's tag: a language, then an optional script and region
/// (`en`, `pt-BR`, `zh-Hant-TW`), so it is also a safe file name.
const TAG = /^[a-z]{2,3}(?:-[A-Z][a-z]{3})?(?:-[A-Z]{2})?$/;

/// A system language prefix in the list: a language and up to three more
/// subtags, in any case.
const SYSTEM_PREFIX = /^[A-Za-z]{2,3}(?:-[A-Za-z0-9]{2,8}){0,3}$/;

const isObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

/// The content of `languages.json` as a list, or null when it is not one:
/// only `fallback` and `languages`; each language only a `tag` of the form
/// above, listed once, and a non-empty `system` of prefixes, none listed
/// twice in the whole list; the fallback one of the languages.
export function readLanguageList(value: unknown): LanguageList | null {
  if (!isObject(value) || Object.keys(value).sort().join(" ") !== "fallback languages") return null;
  const { fallback, languages } = value;
  if (typeof fallback !== "string" || !Array.isArray(languages)) return null;
  const tags = new Set<string>();
  const prefixes = new Set<string>();
  const out: ListedLanguage[] = [];
  for (const entry of languages as unknown[]) {
    if (!isObject(entry) || Object.keys(entry).sort().join(" ") !== "system tag") return null;
    const { tag, system } = entry;
    if (typeof tag !== "string" || !TAG.test(tag) || tags.has(tag)) return null;
    if (!Array.isArray(system) || system.length === 0) return null;
    const own: string[] = [];
    for (const prefix of system as unknown[]) {
      if (typeof prefix !== "string" || !SYSTEM_PREFIX.test(prefix)) return null;
      const folded = prefix.toLowerCase();
      if (prefixes.has(folded)) return null;
      prefixes.add(folded);
      own.push(prefix);
    }
    tags.add(tag);
    out.push({ tag, system: own });
  }
  return tags.has(fallback) ? { fallback, languages: out } : null;
}

/// Adds every string under `value` to `out` by its dotted key; false when
/// something in it is not an object or a string.
function flatten(value: unknown, prefix: string, out: Map<string, string>): boolean {
  if (typeof value === "string") {
    out.set(prefix, value);
    return true;
  }
  if (!isObject(value)) return false;
  return Object.entries(value).every(([key, inner]) => flatten(inner, prefix === "" ? key : `${prefix}.${key}`, out));
}

/// A language file's content as a language, or null when it is not one:
/// only objects and strings, a `_meta.tag` equal to `tag` (its file name)
/// and a `_meta.name`.
export function readLanguage(tag: string, value: unknown, system: readonly string[] = []): Language | null {
  const strings = new Map<string, string>();
  if (!flatten(value, "", strings)) return null;
  const name = strings.get("_meta.name");
  if (strings.get("_meta.tag") !== tag || name === undefined || name.trim() === "") return null;
  return { tag, name, system, strings };
}

/// The listed languages there is a valid file for, in the list's order,
/// from the files by name (`zh-TW.json`). A file the list does not name is
/// left out, and so is a listed language without a valid file; the tests
/// say when either happens.
export function languagesFrom(list: LanguageList | null, files: Readonly<Record<string, unknown>>): Language[] {
  return (list?.languages ?? []).flatMap((listed) => {
    const name = `${listed.tag}.json`;
    const language = Object.hasOwn(files, name) ? readLanguage(listed.tag, files[name], listed.system) : null;
    return language === null ? [] : [language];
  });
}

/// The files of `src/lang/` by name; `languages.json` is not a language.
const FILES: Readonly<Record<string, unknown>> = Object.fromEntries(
  Object.entries(import.meta.glob<unknown>("../lang/*.json", { eager: true, import: "default" }))
    .map(([path, value]) => [path.slice(path.lastIndexOf("/") + 1), value] as const)
    .filter(([name]) => name !== "languages.json"),
);

/// The list as read from `languages.json`, or null when it is broken.
export const LANGUAGE_LIST: LanguageList | null = readLanguageList(listFile);

/// Every listed language there is a file for, in the picker's order.
export const LANGUAGES: readonly Language[] = languagesFrom(LANGUAGE_LIST, FILES);

const fallback: Language = LANGUAGES.find((l) => l.tag === FALLBACK_TAG) ??
  // The file is imported above, so this only happens if it or the list is
  // broken; their tests say so.
  readLanguage(FALLBACK_TAG, en) ?? { tag: FALLBACK_TAG, name: FALLBACK_TAG, system: [], strings: new Map() };

let current: Language = fallback;

/// The language a tag names, or the fallback for one there is no file for.
export function findLanguage(tag: string | null | undefined): Language {
  return LANGUAGES.find((l) => l.tag === tag) ?? fallback;
}

/// A system language's subtags in lower case (`zh-hant-tw`), or null when
/// it is not a language: a POSIX locale reads without its code set and
/// modifier (`zh_TW.UTF-8@euro` is `zh-tw`), and `C` and `POSIX` are none.
export function localeSubtags(locale: string): string[] | null {
  const tag = (locale.split(/[.@]/, 1)[0] ?? "").replaceAll("_", "-");
  if (!/^[A-Za-z]{2,3}(?:-[A-Za-z0-9]{1,8}){0,7}$/.test(tag)) return null;
  return tag.toLowerCase().split("-");
}

/// The language for the system's languages, most preferred first: for the
/// first one any language stands for, the language whose prefix matches
/// it most closely (`zh-Hant-HK` is Traditional Chinese through
/// `zh-Hant`, plain `zh` is Simplified); null when none matches.
export function systemLanguage(
  locales: readonly string[],
  languages: readonly Language[] = LANGUAGES,
): Language | null {
  for (const locale of locales) {
    const subtags = localeSubtags(locale);
    if (subtags === null) continue;
    let best: Language | null = null;
    let length = 0;
    for (const l of languages) {
      for (const prefix of l.system) {
        const want = prefix.toLowerCase().split("-");
        if (want.length > length && want.every((part, i) => subtags[i] === part)) {
          best = l;
          length = want.length;
        }
      }
    }
    if (best !== null) return best;
  }
  return null;
}

/// The system's languages as the shell or the webview reports them, or
/// null when the answer is not a list of them: at most 16 strings of at
/// most 64 characters. Each is still only a candidate for `localeSubtags`.
export function asLocales(answer: unknown): string[] | null {
  if (!Array.isArray(answer) || answer.length > 16) return null;
  const out: string[] = [];
  for (const item of answer as unknown[]) {
    if (typeof item !== "string" || item.length > 64) return null;
    out.push(item);
  }
  return out;
}

/// The language a choice gives: the chosen language; for the system
/// choice, or a tag there is no file for, the system's (`systemLanguage`),
/// and English when the system's languages have no file.
export function chooseLanguage(choice: string | null | undefined, locales: readonly string[]): Language {
  const chosen = choice === SYSTEM_CHOICE ? undefined : LANGUAGES.find((l) => l.tag === choice);
  return chosen ?? systemLanguage(locales) ?? fallback;
}

/// The language the window speaks now.
export function language(): Language {
  return current;
}

/// Switches the window's language; an unknown tag gives the fallback.
/// Returns the language now spoken.
export function setLanguage(tag: string | null | undefined): Language {
  current = findLanguage(tag);
  return current;
}

/// One piece of a text: literal text, or a placeholder's name.
type Piece = { text: string } | { name: string };

/// Splits a text into literal text and placeholders. A brace that does not
/// open a placeholder or a literal pair is kept as it is.
function pieces(text: string): Piece[] {
  const out: Piece[] = [];
  const re = /\{\{|\}\}|\{([a-z_]{1,24})\}/g;
  let at = 0;
  for (let m = re.exec(text); m !== null; m = re.exec(text)) {
    if (m.index > at) out.push({ text: text.slice(at, m.index) });
    out.push(m[1] !== undefined ? { name: m[1] } : { text: m[0][0]! });
    at = m.index + m[0].length;
  }
  if (at < text.length) out.push({ text: text.slice(at) });
  return out;
}

/// The placeholder names a text uses, in order.
export function placeholders(text: string): string[] {
  return pieces(text).flatMap((p) => ("name" in p ? [p.name] : []));
}

/// The text with each placeholder replaced by its value. A placeholder
/// `params` has no value for stays as it is written, so a missing value
/// shows instead of vanishing.
export function fill(text: string, params: Params = {}): string {
  return joined(pieces(text), params);
}

function joined(parts: readonly Piece[], params: Params): string {
  return parts
    .map((p) => ("name" in p ? (Object.hasOwn(params, p.name) ? String(params[p.name]) : `{${p.name}}`) : p.text))
    .join("");
}

/// The text of `key` in the window's language, filled from `params`, cut
/// where its placeholder `{name}` first stands: the words before it and
/// after it, so the caller can put something of its own there (the
/// header's coloured counts). Null when the text has no such placeholder.
export function around(
  key: Key,
  name: string,
  params: Params = {},
  lang: Language = current,
): readonly [string, string] | null {
  const parts = pieces(raw(key, lang));
  const at = parts.findIndex((p) => "name" in p && p.name === name);
  if (at < 0) return null;
  return [joined(parts.slice(0, at), params), joined(parts.slice(at + 1), params)];
}

/// The raw text of `key` in `lang`, falling back to English, and to the
/// key itself when neither has it.
export function raw(key: Key, lang: Language = current): string {
  return lang.strings.get(key) ?? fallback.strings.get(key) ?? key;
}

/// The text of `key` in the window's language, its placeholders filled
/// from `params`.
export function t(key: Key, params?: Params): string {
  return fill(raw(key), params);
}

/// The keys of one section: what is directly under `section.` in the
/// English file.
export function sectionKeys(section: string): string[] {
  const prefix = `${section}.`;
  return [...fallback.strings.keys()]
    .filter((k) => k.startsWith(prefix) && !k.slice(prefix.length).includes("."))
    .map((k) => k.slice(prefix.length));
}

/// A table of the texts of one section, `section.<name>` for each name in
/// `shape` (the English file's section, which a caller checks against a
/// contract union with `satisfies`), read in the current language every
/// time a property is read.
export function table<T extends Readonly<Record<string, string>>>(section: string, shape: T): Readonly<T> {
  const out = {} as Record<string, string>;
  for (const name of Object.keys(shape)) {
    Object.defineProperty(out, name, { enumerable: true, get: () => t(`${section}.${name}` as Key) });
  }
  return Object.freeze(out) as Readonly<T>;
}

/// The English file, for `satisfies` checks against the contract.
export const REFERENCE = en;
