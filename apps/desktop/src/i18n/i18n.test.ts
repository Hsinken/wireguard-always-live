// The language files: the list (`languages.json`) and the files agree,
// every file has the keys and placeholders of English, the fallback, its
// notification templates pass the contract's rules, the system's
// languages pick the right file, and no user-visible text is left in the
// code outside the language folder. Generic over every file there is.

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import {
  FALLBACK_TAG,
  LANGUAGES,
  LANGUAGE_LIST,
  SYSTEM_CHOICE,
  around,
  asLocales,
  chooseLanguage,
  type Key,
  type Language,
  fill,
  findLanguage,
  languagesFrom,
  language,
  localeSubtags,
  placeholders,
  raw,
  readLanguage,
  readLanguageList,
  sectionKeys,
  setLanguage,
  systemLanguage,
  t,
} from ".";
import { TEMPLATE_PLACEHOLDERS } from "../protocol";
import {
  MAX_TEMPLATES_ID_BYTES,
  MAX_TEMPLATE_BYTES,
  MAX_UNNAMED_DEVICE_BYTES,
  TEMPLATE_FIELDS,
  templateProblem,
  templateTexts,
} from "../templates";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const SRC = join(HERE, "..");
const LANG = join(SRC, "lang");
const APP = join(SRC, "..");
const read = (path: string) => readFileSync(path, "utf8");

/// Ideographs, CJK punctuation, kana, Hangul and full-width forms.
const CJK = /[　-〿぀-ヿ㐀-䶿一-鿿가-힯豈-﫿＀-￯]/u;

const reference = findLanguage(FALLBACK_TAG);

describe("the language files", () => {
  it("are every JSON file of the folder but the list, each listed once and valid", () => {
    const files = readdirSync(LANG).filter((f) => f.endsWith(".json") && f !== "languages.json");
    expect(LANGUAGE_LIST).not.toBeNull();
    expect(LANGUAGE_LIST?.fallback).toBe(FALLBACK_TAG);
    // Every listed language has a valid file, and every file is listed.
    expect(LANGUAGES.map((l) => l.tag)).toEqual(LANGUAGE_LIST?.languages.map((l) => l.tag));
    expect(LANGUAGES.map((l) => `${l.tag}.json`).sort()).toEqual(files.sort());
    expect(reference.tag).toBe("en");
    expect(reference.name).toBe("English");
    for (const l of LANGUAGES) expect(l.strings.get("_meta.tag")).toBe(l.tag);
  });

  it("include the owner's languages, in the owner's order (request of 2026-11-05)", () => {
    // Another language may be added anywhere in the list (Codex P3): only
    // these must be there, in this order among themselves.
    const owners = [
      "zh-TW",
      "en",
      "zh-CN",
      "ja",
      "ko",
      "de",
      "es",
      "pt-BR",
      "tr",
      "fr",
      "ru",
      "id",
      "vi",
    ];
    expect(LANGUAGES.map((l) => l.tag).filter((tag) => owners.includes(tag))).toEqual(owners);
    // Each is named in its own language, and no two alike.
    expect(new Set(LANGUAGES.map((l) => l.name)).size).toBe(LANGUAGES.length);
    expect(findLanguage("zh-TW").name).toBe("繁體中文");
  });

  it("are each described in the folder's README", () => {
    const readme = read(join(LANG, "README.md"));
    for (const l of LANGUAGES) expect(readme, l.tag).toContain(`\`${l.tag}\``);
  });

  it("each have exactly the reference's keys, with the same placeholders", () => {
    const keys = [...reference.strings.keys()].sort();
    for (const l of LANGUAGES) {
      expect([...l.strings.keys()].sort(), l.tag).toEqual(keys);
      for (const key of keys) {
        if (key.startsWith("_meta.")) continue;
        const want = placeholders(raw(key as never, reference)).sort();
        expect(placeholders(raw(key as never, l)).sort(), `${l.tag} ${key}`).toEqual(want);
      }
    }
  });

  it("hold no empty text and no stray brace", () => {
    for (const l of LANGUAGES) {
      for (const [key, text] of l.strings) {
        expect(text.length, `${l.tag} ${key}`).toBeGreaterThan(0);
        const stripped = text.replace(/\{\{|\}\}|\{[a-z_]{1,24}\}/g, "");
        expect(stripped, `${l.tag} ${key}`).not.toMatch(/[{}]/);
      }
    }
  });

  it("have notification templates the watchdog takes, and en's are the contract's built-in set", () => {
    for (const l of LANGUAGES) {
      const texts = templateTexts(l);
      for (const field of TEMPLATE_FIELDS) {
        expect(templateProblem(field, texts[field]), `${l.tag} ${field}`).toBeNull();
      }
    }
    const fixture = JSON.parse(
      read(join(APP, "../../crates/wgal-protocol/fixtures/request_set_notification_templates.json")),
    ) as { request: { templates: Record<string, string> } };
    const { id: _id, ...builtIn } = fixture.request.templates;
    expect(templateTexts(findLanguage("en"))).toEqual(builtIn);
    expect(Object.keys(builtIn).sort()).toEqual([...TEMPLATE_FIELDS].sort());
  });

  it("give the Rust shell the tray texts it reads", () => {
    const lang = read(join(APP, "src-tauri/src/lang.rs"));
    const keys = [...lang.matchAll(/=> "([a-z_]+)",/g)].map((m) => m[1]);
    expect(keys.length).toBe(18);
    expect(keys.sort()).toEqual(sectionKeys("tray").sort());
  });
});

describe("t", () => {
  it("fills placeholders, keeps literal braces and shows a missing value", () => {
    expect(fill("{a} and {b}", { a: 1, b: "x" })).toBe("1 and x");
    expect(fill("{{a}} {a}", { a: 2 })).toBe("{a} 2");
    expect(fill("{missing}", {})).toBe("{missing}");
    expect(fill("{a}", { a: "{b}", b: "no" })).toBe("{b}");
    // Only own values count.
    expect(fill("{constructor}", {})).toBe("{constructor}");
    expect(placeholders("x {a} {{b}} {c_d}")).toEqual(["a", "c_d"]);
    expect(t("probe.refused", { ms: 12 })).toBe("連線被拒（12 ms，通路可用）");
  });

  it("falls back to English for a key another language lacks, and for an unknown tag", () => {
    const other = readLanguage("xx", { _meta: { tag: "xx", name: "Test" }, tabs: { log: "Kayıt" } }) as Language;
    expect(other).not.toBeNull();
    expect(raw("tabs.log", other)).toBe("Kayıt");
    expect(raw("tabs.about", other)).toBe("About");
    expect(findLanguage("xx-YY")).toBe(reference);
    expect(findLanguage(null)).toBe(reference);
    expect(setLanguage("no-such-tag")).toBe(reference);
    expect(language()).toBe(reference);
    setLanguage("zh-TW");
  });

  it("reads only well-formed files named by their own tag", () => {
    expect(readLanguage("xx", { _meta: { tag: "yy", name: "Test" } })).toBeNull();
    expect(readLanguage("xx", { _meta: { tag: "xx", name: " " } })).toBeNull();
    expect(readLanguage("xx", { _meta: { tag: "xx", name: "Test" }, a: [1] })).toBeNull();
    expect(readLanguage("xx", { _meta: { tag: "xx", name: "Test" }, a: 1 })).toBeNull();
    const list = readLanguageList({
      fallback: "en",
      languages: [
        { tag: "zh-TW", system: ["zh-TW"] },
        { tag: "en", system: ["en"] },
        { tag: "de", system: ["de"] },
        { tag: "fr", system: ["fr"] },
      ],
    });
    const found = languagesFrom(list, {
      "en.json": { _meta: { tag: "en", name: "English" } },
      "zh-TW.json": { _meta: { tag: "zh-TW", name: "繁體中文" } },
      // Not listed, broken, or named for another tag: left out.
      "ja.json": { _meta: { tag: "ja", name: "日本語" } },
      "de.json": "not an object",
      "fr.json": { _meta: { tag: "de", name: "Deutsch" } },
    });
    // In the list's order.
    expect(found.map((l) => l.tag)).toEqual(["zh-TW", "en"]);
    expect(found[0]?.system).toEqual(["zh-TW"]);
    expect(languagesFrom(null, { "en.json": { _meta: { tag: "en", name: "English" } } })).toEqual([]);
  });

  it("reads only a well-formed list", () => {
    const ok = { fallback: "en", languages: [{ tag: "en", system: ["en"] }] };
    expect(readLanguageList(ok)).toEqual(ok);
    for (const bad of [
      null,
      [],
      { languages: ok.languages },
      { ...ok, extra: 1 },
      { ...ok, fallback: "de" },
      { fallback: "en", languages: [{ tag: "en", system: ["en"], name: "English" }] },
      { fallback: "en", languages: [{ tag: "en", system: [] }] },
      { fallback: "en", languages: [{ tag: "en", system: "en" }] },
      { fallback: "en", languages: [{ tag: "EN", system: ["en"] }] },
      { fallback: "en", languages: [{ tag: "../en", system: ["en"] }] },
      { fallback: "en", languages: [{ tag: "en", system: ["en_US"] }] },
      { fallback: "en", languages: [{ tag: "en", system: ["en"] }, { tag: "en", system: ["en-GB"] }] },
      // A system language two languages claim.
      { fallback: "en", languages: [{ tag: "en", system: ["en"] }, { tag: "de", system: ["EN"] }] },
    ]) {
      expect(readLanguageList(bad), JSON.stringify(bad)).toBeNull();
    }
  });
});

describe("the system's language (owner request of 2026-11-05)", () => {
  const pick = (...locales: string[]) => systemLanguage(locales)?.tag ?? null;

  it("is matched by the closest prefix, script and region included", () => {
    expect(pick("zh-TW")).toBe("zh-TW");
    expect(pick("zh-HK")).toBe("zh-TW");
    expect(pick("zh-MO")).toBe("zh-TW");
    expect(pick("zh-Hant")).toBe("zh-TW");
    expect(pick("zh-Hant-HK")).toBe("zh-TW");
    expect(pick("zh-Hant-TW")).toBe("zh-TW");
    expect(pick("zh")).toBe("zh-CN");
    expect(pick("zh-CN")).toBe("zh-CN");
    expect(pick("zh-SG")).toBe("zh-CN");
    expect(pick("zh-Hans-TW")).toBe("zh-CN");
    expect(pick("zh-Hans-CN")).toBe("zh-CN");
    expect(pick("pt-BR")).toBe("pt-BR");
    expect(pick("pt-PT")).toBe("pt-BR");
    expect(pick("en-GB")).toBe("en");
    expect(pick("de-AT")).toBe("de");
    expect(pick("es-419")).toBe("es");
    expect(pick("fr-CA")).toBe("fr");
    expect(pick("in-ID")).toBe("id");
    expect(pick("ja-JP")).toBe("ja");
    expect(pick("ko-KR")).toBe("ko");
    expect(pick("ru-RU")).toBe("ru");
    expect(pick("tr-TR")).toBe("tr");
    expect(pick("vi-VN")).toBe("vi");
  });

  it("reads POSIX locales and skips what is not a language", () => {
    expect(localeSubtags("zh_TW.UTF-8")).toEqual(["zh", "tw"]);
    expect(localeSubtags("de_DE@euro")).toEqual(["de", "de"]);
    for (const none of ["C", "POSIX", "", "x", "../en", "en US", "e1"]) expect(localeSubtags(none), none).toBeNull();
    expect(pick("ja_JP.UTF-8")).toBe("ja");
    expect(pick("C", "POSIX", "ko_KR.UTF-8")).toBe("ko");
  });

  it("takes the first language there is a file for, and none when there is none", () => {
    expect(pick("it-IT", "de-DE", "en-US")).toBe("de");
    expect(pick("it-IT", "nl-NL")).toBeNull();
    expect(pick()).toBeNull();
    // A language prefix never matches a longer language code.
    expect(pick("eng")).toBeNull();
  });

  it("gives the chosen language, the system's, or English", () => {
    expect(chooseLanguage("ja", ["zh-TW"]).tag).toBe("ja");
    expect(chooseLanguage(SYSTEM_CHOICE, ["zh-TW"]).tag).toBe("zh-TW");
    expect(chooseLanguage(SYSTEM_CHOICE, ["it-IT"]).tag).toBe("en");
    expect(chooseLanguage(SYSTEM_CHOICE, []).tag).toBe("en");
    // A stored tag there is no file for follows the system.
    expect(chooseLanguage("xx-YY", ["ko-KR"]).tag).toBe("ko");
    expect(chooseLanguage(null, ["fr-FR"]).tag).toBe("fr");
  });

  it("comes from the shell or the webview only as a short list of short strings", () => {
    expect(asLocales(["zh-TW", "en-US"])).toEqual(["zh-TW", "en-US"]);
    expect(asLocales([])).toEqual([]);
    for (const bad of [null, "zh-TW", [1], ["x".repeat(65)], Array.from({ length: 17 }, () => "en")]) {
      expect(asLocales(bad), JSON.stringify(bad)).toBeNull();
    }
  });
});

describe("the code", () => {
  /// Every `.ts` file under `src/`, but this folder, the tests, the dev fake
  /// and the generated contract (whose comments quote the plan).
  function sources(dir: string): string[] {
    return readdirSync(dir).flatMap((name) => {
      const path = join(dir, name);
      if (statSync(path).isDirectory()) {
        return ["i18n", "dev", "generated"].includes(name) ? [] : sources(path);
      }
      return name.endsWith(".ts") && !name.endsWith(".test.ts") ? [path] : [];
    });
  }

  it("has no user-visible text of its own: no CJK outside the language files", () => {
    const files = [...sources(SRC), join(APP, "index.html")];
    expect(files.length).toBeGreaterThan(20);
    for (const file of files) {
      read(file)
        .split("\n")
        .forEach((line, i) => expect(CJK.test(line), `${file}:${i + 1}: ${line}`).toBe(false));
    }
  });

  it("uses only keys the reference has in index.html", () => {
    const html = read(join(APP, "index.html"));
    const keys = [...html.matchAll(/data-i18n(?:-[a-z-]+)?="([^"]+)"/g)].map((m) => m[1]!);
    expect(keys.length).toBeGreaterThan(50);
    for (const key of keys) expect(reference.strings.has(key), key).toBe(true);
  });

  it("states the contract's template limits", () => {
    const rust = read(join(APP, "../../crates/wgal-protocol/src/template.rs"));
    expect(rust).toContain(`pub const MAX_BYTES: usize = ${MAX_TEMPLATE_BYTES};`);
    expect(rust).toContain(`pub const MAX_BYTES: usize = ${MAX_TEMPLATES_ID_BYTES};`);
    expect(rust).toContain("pub const MAX_UNNAMED_DEVICE_BYTES: usize = DisplayName::MAX_BYTES;");
    const text = read(join(APP, "../../crates/wgal-protocol/src/text.rs"));
    expect(text).toMatch(new RegExp(`DisplayName,\\s*"display name",\\s*${MAX_UNNAMED_DEVICE_BYTES}\\s*\\)`));
    expect(Object.keys(TEMPLATE_PLACEHOLDERS)).toEqual(TEMPLATE_FIELDS);
  });
});

describe("button words (owner request of 2026-11-04)", () => {
  // What a button or a tray menu item says, in every language: never
  // ending in an ellipsis, which reads oddly on a button. The buttons of
  // index.html, those the code makes with their words, and those whose
  // words the code sets later.
  const html = read(join(APP, "index.html"));
  const fromHtml = [...html.matchAll(/<button\b[^>]*\bdata-i18n="([^"]+)"/g)].map((m) => m[1]!);
  const code = (function walk(dir: string): string[] {
    return readdirSync(dir).flatMap((name) => {
      const path = join(dir, name);
      if (statSync(path).isDirectory()) return ["dev", "generated"].includes(name) ? [] : walk(path);
      return name.endsWith(".ts") && !name.endsWith(".test.ts") ? [read(path)] : [];
    });
  })(SRC).join("\n");
  // `actionButton("key"…)`, a card's `button("key"…)`, `el("button", { key: "key" … })`.
  const made = [
    ...code.matchAll(/\b(?:actionButton|button)\("([a-z_]+\.[a-z_.]+)"/g),
    ...code.matchAll(/el\("button", \{[^}]*\bkey: "([a-z_]+\.[a-z_.]+)"/g),
  ].map((m) => m[1]!);
  // Busy words, a switch's two states, a row's action, the tray's items.
  const relabelled = [
    "discovered.register",
    "discovered.registered",
    "discovered.relink",
    "export.busy",
    "log.loading",
    "log.more",
    "settings.pause_all",
    "settings.resume_all",
    "tray.menu_open",
    "tray.menu_pause",
    "tray.menu_quit",
    "tray.menu_stop",
  ];

  it("has no ellipsis", () => {
    expect(fromHtml).toContain("tunnels.discover");
    expect(fromHtml).toContain("export.button");
    for (const key of ["card.connect", "card.disconnect", "card.probe_now", "card.reconnect", "card.remove", "common.reset"]) {
      expect(made).toContain(key);
    }
    for (const lang of LANGUAGES) {
      for (const key of [...fromHtml, ...made, ...relabelled]) {
        const text = lang.strings.get(key as Key);
        expect(text, `${lang.tag} ${key}`).toBeTypeOf("string");
        expect(text, `${lang.tag} ${key}`).not.toMatch(/(…|\.\.\.)\s*$/);
      }
    }
  });
});

describe("a text cut at its placeholder (Codex P3-N12)", () => {
  const lang = (counts: string) =>
    readLanguage("xx", { _meta: { tag: "xx", name: "Test" }, summary: { counts } }) as Language;

  it("gives the words on either side, wherever the placeholder stands", () => {
    expect(around("summary.counts", "lights", { count: 2 })).toEqual(["2 條隧道：", ""]);
    expect(around("summary.counts", "lights", { count: 3 }, lang("{lights} in {count} tunnels."))).toEqual([
      "",
      " in 3 tunnels.",
    ]);
    expect(around("summary.counts", "lights", { count: 3 }, lang("Total {count}: {lights}."))).toEqual([
      "Total 3: ",
      ".",
    ]);
  });

  it("keeps any character of the text, and escaped braces", () => {
    expect(around("summary.counts", "lights", { count: 1 }, lang("A\u0000{count}: {lights} {{x}} \u0000."))).toEqual([
      "A\u00001: ",
      " {x} \u0000.",
    ]);
  });

  it("says when the placeholder is not there", () => {
    expect(around("summary.counts", "lights", { count: 1 }, lang("{count} tunnels"))).toBeNull();
  });
});
