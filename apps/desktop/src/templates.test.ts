// The Telegram message templates the window hands the watchdog: the
// contract's rules, the set's name, when it is sent, and the preview.

import { describe, expect, it } from "vitest";

import { findLanguage } from "./i18n";
import type { NotificationTemplates, Snapshot } from "./protocol";
import {
  HASH_SEPARATOR,
  TEMPLATE_FIELDS,
  TemplateSync,
  fnv1a32,
  messageTimes,
  messageZone,
  previewMessages,
  renderTemplate,
  tagOf,
  templateProblem,
  templateTexts,
  templateUse,
  templateUseText,
  templatesFor,
  templatesId,
  templatesIdProblem,
} from "./templates";
import { messageDeviceName } from "./view";

const zh = findLanguage("zh-TW");
const texts = templateTexts(zh);

describe("the set's name", () => {
  it("is FNV-1a 32 of every text", () => {
    // The algorithm's published test values.
    expect(fnv1a32("").toString(16)).toBe("811c9dc5");
    expect(fnv1a32("a").toString(16)).toBe("e40c292c");
    expect(fnv1a32("foobar").toString(16)).toBe("bf9cf968");
    // Over UTF-8: "測" is three bytes.
    const overBytes = (bytes: number[]) => bytes.reduce((h, b) => Math.imul(h ^ b, 0x01000193) >>> 0, 0x811c9dc5);
    expect(fnv1a32("測")).toBe(overBytes([0xe6, 0xb8, 0xac]));
    // In the contract's order.
    const input = [
      texts.formal,
      texts.result_failed,
      texts.result_not_executed,
      texts.retry_limited,
      texts.retry_forever,
      texts.retry_none,
      texts.exhausted,
      texts.recovered,
      texts.test,
      texts.unnamed_device,
    ].join("\n\u0000");
    expect(TEMPLATE_FIELDS).toEqual([
      "formal",
      "result_failed",
      "result_not_executed",
      "retry_limited",
      "retry_forever",
      "retry_none",
      "exhausted",
      "recovered",
      "test",
      "unnamed_device",
    ]);
    expect(HASH_SEPARATOR).toBe("\n\u0000");
    expect(templatesId("zh-TW", texts)).toBe(`zh-TW/${fnv1a32(input).toString(16).padStart(8, "0")}`);
    expect(templatesId("zh-TW", texts)).toBe("zh-TW/f967042d");
  });

  it("changes with any text, and with a text moved to another field", () => {
    const id = templatesId("zh-TW", texts);
    for (const field of Object.keys(texts) as Array<keyof typeof texts>) {
      expect(templatesId("zh-TW", { ...texts, [field]: `${texts[field]}!` }), field).not.toBe(id);
    }
    const swapped = { ...texts, result_failed: texts.result_not_executed, result_not_executed: texts.result_failed };
    expect(templatesId("zh-TW", swapped)).not.toBe(id);
    expect(templatesId("en", texts)).toBe(`en/${id.slice("zh-TW/".length)}`);
    expect(templatesIdProblem(id)).toBeNull();
  });

  it("is checked by the contract's rules", () => {
    expect(templatesIdProblem("")).not.toBeNull();
    expect(templatesIdProblem("a".repeat(49))).not.toBeNull();
    expect(templatesIdProblem("a".repeat(48))).toBeNull();
    expect(templatesIdProblem("/zh-TW")).not.toBeNull();
    expect(templatesIdProblem("zh TW/1")).not.toBeNull();
    expect(templatesIdProblem("zh-TW/1.2_3@x")).toBeNull();
    expect(tagOf("zh-TW/c8d3319c")).toBe("zh-TW");
    expect(tagOf("custom")).toBe("custom");
  });

  it("names a set of the window's language that the watchdog takes", () => {
    const { set, problems } = templatesFor(zh);
    expect(problems).toEqual([]);
    expect(set).toEqual({ id: "zh-TW/f967042d", ...texts });
  });
});

describe("the contract's template rules", () => {
  it("take the reference texts and refuse what the watchdog refuses", () => {
    expect(templateProblem("formal", texts.formal)).toBeNull();
    const formal = "{device} {tunnel} {incident} {time_zone} {started_at} {result} {retry}";
    expect(templateProblem("formal", formal)).toBeNull();
    expect(templateProblem("formal", formal.replace(" {retry}", ""))).not.toBeNull();
    // The form before watchdog 1.3.0, each time followed by its zone.
    expect(templateProblem("formal", formal.replace(" {time_zone}", ""))).not.toBeNull();
    expect(templateProblem("test", "{device} {time}")).not.toBeNull();
    expect(templateProblem("test", "{device} {time_zone} {time}")).toBeNull();
    expect(templateProblem("retry_limited", "{interval} {limit}")).toBeNull();
    expect(templateProblem("retry_limited", "{interval}")).not.toBeNull();
    expect(templateProblem("retry_forever", "{interval} {limit}")).not.toBeNull();
    expect(templateProblem("retry_none", "{interval}")).not.toBeNull();
    expect(templateProblem("exhausted", "{device} {tunnel} {incident} {time_zone} {started_at} {retries}")).toBeNull();
    expect(templateProblem("exhausted", "{device} {tunnel} {incident} {time_zone} {started_at}")).not.toBeNull();
    expect(templateProblem("exhausted", "{device} {tunnel} {incident} {started_at} {retries}")).not.toBeNull();
    expect(
      templateProblem("recovered", "{device} {tunnel} {incident} {time_zone} {started_at} {recovered_at}"),
    ).toBeNull();
    expect(templateProblem("recovered", "{device} {tunnel} {incident} {time_zone} {started_at} {retries}")).not.toBeNull();
    expect(templateProblem("recovered", "{device} {tunnel} {incident} {started_at} {recovered_at}")).not.toBeNull();
    expect(templateProblem("formal", "{{literal}} " + formal)).toBeNull();
    for (const bad of [
      "",
      " " + formal,
      formal + "\n",
      formal + "\t",
      formal + "\r\nx",
      formal.replace("{result}", ""),
      formal + " {device}",
      formal + " {token}",
      formal + " {endpoint}",
      formal + " {",
      formal + " }",
      formal + " {Device}",
      formal + " { device }",
      "x".repeat(1025),
    ]) {
      expect(templateProblem("formal", bad), JSON.stringify(bad)).not.toBeNull();
    }
    // 1024 bytes is the limit in UTF-8, not in characters.
    expect(templateProblem("result_failed", "測".repeat(341))).toBeNull();
    expect(templateProblem("result_failed", "測".repeat(342))).not.toBeNull();
    expect(templateProblem("result_failed", "{device}")).not.toBeNull();
    expect(templateProblem("test", "測試 {device} {time_zone} {time}")).toBeNull();
    expect(templateProblem("test", "測試 {device} {time}")).not.toBeNull();
    expect(templateProblem("test", "測試 {device}")).not.toBeNull();
    expect(templateProblem("unnamed_device", "（未命名）")).toBeNull();
    expect(templateProblem("unnamed_device", "a\nb")).not.toBeNull();
    expect(templateProblem("unnamed_device", "x".repeat(97))).not.toBeNull();
  });

  it("render as the watchdog does", () => {
    expect(renderTemplate("{{a}} {a} {b}", { a: "1", b: "{a}" })).toBe("{a} 1 {a}");
    expect(renderTemplate("{a}", {})).toBe("");
    expect(renderTemplate("{constructor}", {})).toBe("");
  });
});

describe("when the window sends its set", () => {
  const ours: NotificationTemplates = { id: "zh-TW/c8d3319c", ...texts };

  it("sends it once over the built-in set, and not again for the same pair", () => {
    const sync = new TemplateSync();
    expect(sync.next(null, ours)).toEqual({ set: ours, asked: false });
    // Refused, say: the next snapshot still shows the built-in set.
    expect(sync.next(null, ours)).toBeNull();
    expect(sync.next(null, ours)).toBeNull();
  });

  it("sends nothing until the window's language is settled, then the settled one's set (Codex P2)", () => {
    // Following the system: the webview guessed English, the shell then
    // said Traditional Chinese. The snapshot came first.
    const guess: NotificationTemplates = { ...ours, id: "en/12345678" };
    for (const order of ["snapshot first", "request first"]) {
      const sync = new TemplateSync();
      sync.languageSettled = false;
      if (order === "request first") sync.request();
      expect(sync.next(null, guess), order).toBeNull();
      expect(sync.next(null, guess), order).toBeNull();
      sync.languageSettled = true;
      // The set of the language the window settled on goes, once, and a
      // request made meanwhile is answered by it.
      expect(sync.next(null, ours), order).toEqual({ set: ours, asked: order === "request first" });
      expect(sync.next(null, ours), order).toBeNull();
    }
  });

  it("sends nothing while the watchdog has it, and nothing while a send is under way", () => {
    const sync = new TemplateSync();
    expect(sync.next(ours.id, ours)).toBeNull();
    sync.sending = true;
    expect(sync.next(null, ours)).toBeNull();
    sync.sending = false;
    expect(sync.next(null, ours)).toEqual({ set: ours, asked: false });
  });

  it("replaces an older version of its own language once", () => {
    const sync = new TemplateSync();
    expect(sync.next("zh-TW/00000000", ours)).toEqual({ set: ours, asked: false });
    expect(sync.next("zh-TW/00000000", ours)).toBeNull();
  });

  it("never overwrites another language by itself, only when the user asks", () => {
    const sync = new TemplateSync();
    for (const other of ["en/12345678", "ja-JP/abcdef01", "custom"]) {
      expect(sync.next(other, ours), other).toBeNull();
      expect(sync.next(other, ours), other).toBeNull();
    }
    sync.request();
    expect(sync.next("en/12345678", ours)).toEqual({ set: ours, asked: true });
    // Asked once, sent once: the pair is not sent again by itself.
    expect(sync.next("en/12345678", ours)).toBeNull();
    // A request the watchdog already meets sends nothing and is done.
    sync.request();
    expect(sync.next(ours.id, ours)).toBeNull();
    expect(sync.next("en/12345678", ours)).toBeNull();
  });

  it("sends again for a new pair: another language chosen here, or a new set in use", () => {
    const sync = new TemplateSync();
    expect(sync.next(null, ours)).not.toBeNull();
    const other: NotificationTemplates = { ...ours, id: "zh-TW/ffffffff" };
    expect(sync.next(null, other)).toEqual({ set: other, asked: false });
    expect(sync.next(null, other)).toBeNull();
  });

  it("says which set is in use", () => {
    expect(templateUse(undefined, ours.id)).toEqual({ kind: "unknown" });
    expect(templateUse(null, ours.id)).toEqual({ kind: "builtin" });
    expect(templateUse(ours.id, ours.id)).toEqual({ kind: "ours" });
    expect(templateUse("zh-TW/00000000", ours.id)).toEqual({ kind: "ours_changed" });
    expect(templateUse("en/1", ours.id)).toEqual({ kind: "foreign", tag: "en" });
    expect(templateUseText({ kind: "builtin" }, zh)).toBe("使用中的訊息範本：內建");
    expect(templateUseText({ kind: "ours" }, zh)).toBe("使用中的訊息範本：繁體中文");
    // A language by the name its file gives, or by its tag without a file.
    expect(templateUseText({ kind: "foreign", tag: "en" }, zh)).toBe(
      "Telegram 通知使用的語言：English（與目前介面的語言不同）",
    );
    expect(templateUseText({ kind: "foreign", tag: "xx-YY" }, zh)).toBe(
      "Telegram 通知使用的語言：xx-YY（與目前介面的語言不同）",
    );
    expect(templateUseText({ kind: "foreign", tag: "zh-TW" }, zh)).toContain("繁體中文");
  });
});

describe("the preview", () => {
  const taipei = { configured: null, effective: "Asia/Taipei", utc_offset_secs: 28_800 };
  // 2026-11-03 09:16:05 UTC.
  const at = Date.UTC(2026, 10, 3, 9, 16, 5);

  it("writes times and the zone as the watchdog does", () => {
    expect(messageTimes([at] as const, taipei)).toEqual({
      timeZone: "Asia/Taipei, UTC+08:00",
      times: ["2026-11-03 17:16:05"],
    });
    const utc = { configured: "UTC", effective: "UTC", utc_offset_secs: 0 };
    expect(messageTimes([at] as const, utc)).toEqual({ timeZone: "UTC", times: ["2026-11-03 09:16:05"] });
    const unnamed = { configured: null, effective: null, utc_offset_secs: -12_600 };
    expect(messageTimes([at] as const, unnamed)).toEqual({ timeZone: "UTC-03:30", times: ["2026-11-03 05:46:05"] });
    // An offset with seconds keeps them, in the zone line and the time.
    const odd = { configured: null, effective: null, utc_offset_secs: 8 * 3600 + 6 * 60 + 5 };
    expect(messageTimes([at] as const, odd)).toEqual({ timeZone: "UTC+08:06:05", times: ["2026-11-03 17:22:10"] });
    expect(messageZone(at, taipei)).toBe("Asia/Taipei, UTC+08:00");
  });

  it("gives a later time across a daylight saving change its own offset", () => {
    const newYork = { configured: "America/New_York", effective: "America/New_York", utc_offset_secs: -18_000 };
    // 2026-11-01 06:00:00 UTC: clocks go back from 02:00 EDT to 01:00 EST.
    const autumn = Date.UTC(2026, 10, 1, 6, 0, 0);
    expect(messageTimes([autumn - 1000, autumn + 600_000] as const, newYork)).toEqual({
      timeZone: "America/New_York, UTC-04:00",
      times: ["2026-11-01 01:59:59", "2026-11-01 01:10:00 (UTC-05:00)"],
    });
    expect(messageTimes([autumn, autumn + 600_000] as const, newYork)).toEqual({
      timeZone: "America/New_York, UTC-05:00",
      times: ["2026-11-01 01:00:00", "2026-11-01 01:10:00"],
    });
  });

  it("fills every placeholder with the sample values", () => {
    const preview = previewMessages(texts, {
      deviceName: "辦公室筆電",
      tunnel: "Office",
      incident: "inc-000042",
      startedAtMs: at - 300_000,
      nowMs: at,
      zone: taipei,
    });
    expect(preview.formal).toBe(
      [
        "【WG Always Live 異常通知】",
        "裝置：辦公室筆電",
        "隧道：Office",
        "事件：inc-000042",
        "時區：Asia/Taipei, UTC+08:00",
        "異常開始：2026-11-03 17:11:05",
        "復原結果：第一輪自動復原失敗（重新啟動後仍無法通訊）",
        "WG Always Live 會每 60 秒再試一次，最多 100 次；恢復並穩定 2 分鐘後會再通知。",
      ].join("\n"),
    );
    expect(preview.exhausted).toBe(
      [
        "【WG Always Live 停止重試】",
        "裝置：辦公室筆電",
        "隧道：Office",
        "事件：inc-000042",
        "時區：Asia/Taipei, UTC+08:00",
        "異常開始：2026-11-03 17:11:05",
        "已自動重試 100 次，達到上限，停止自動復原。請手動處理；恢復並穩定 2 分鐘後會再通知。",
      ].join("\n"),
    );
    expect(preview.recovered).toBe(
      [
        "【WG Always Live 已恢復】",
        "裝置：辦公室筆電",
        "隧道：Office",
        "事件：inc-000042",
        "時區：Asia/Taipei, UTC+08:00",
        "異常開始：2026-11-03 17:11:05",
        "恢復時間：2026-11-03 17:14:05",
        "通訊已恢復，並穩定 2 分鐘。",
      ].join("\n"),
    );
    // The tunnel's own retry: keep trying, or the rounds a limit allows.
    const forever = previewMessages(texts, {
      deviceName: "x",
      tunnel: "Office",
      incident: "inc-1",
      startedAtMs: at,
      nowMs: at,
      zone: taipei,
      retry: { limit: null, interval_secs: 300 },
    });
    expect(forever.formal.endsWith("WG Always Live 會每 300 秒持續重試；恢復並穩定 2 分鐘後會再通知。")).toBe(true);
    const none = previewMessages(texts, {
      deviceName: "x",
      tunnel: "Office",
      incident: "inc-1",
      startedAtMs: at,
      nowMs: at,
      zone: taipei,
      retry: { limit: 0, interval_secs: 60 },
    });
    expect(none.formal.endsWith("WG Always Live 不會再自動重試，請手動處理；恢復並穩定 2 分鐘後會再通知。")).toBe(true);
    expect(preview.test).toBe(
      "【WG Always Live 測試通知】\n這是測試訊息，不代表任何 VPN 故障。\n裝置：辦公室筆電\n時區：Asia/Taipei, UTC+08:00\n時間：2026-11-03 17:16:05",
    );
    const unnamed = previewMessages(texts, {
      deviceName: "  ",
      tunnel: "Office",
      incident: "inc-1",
      startedAtMs: at,
      nowMs: at,
      zone: taipei,
    });
    expect(unnamed.test).toContain("裝置：（未命名裝置）");
    expect(unnamed.formal).not.toMatch(/\{[a-z_]+\}/);
  });

  it("names the device as the watchdog will: the name, else the host name, else unnamed", () => {
    // As the window fills the preview: the snapshot's names through messageDeviceName.
    const device = (device_name: string, host_name: string | null) =>
      previewMessages(texts, {
        deviceName: messageDeviceName({ device_name, host_name } as Snapshot["notification"]),
        tunnel: "Office",
        incident: "inc-1",
        startedAtMs: at,
        nowMs: at,
        zone: taipei,
      });
    for (const [name, host, said] of [
      ["Lab laptop", "office-pc", "裝置：Lab laptop\n"],
      [" Lab ", "office-pc", "裝置： Lab \n"],
      ["\uFEFF", "office-pc", "裝置：\uFEFF\n"],
      ["", "office-pc", "裝置：office-pc\n"],
      ["\u3000 ", "office-pc", "裝置：office-pc\n"],
      ["", null, "裝置：（未命名裝置）\n"],
      ["  ", null, "裝置：（未命名裝置）\n"],
    ] as const) {
      const preview = device(name, host);
      for (const text of [preview.formal, preview.exhausted, preview.recovered, preview.test]) {
        expect(text, JSON.stringify([name, host])).toContain(said);
      }
    }
  });
});
