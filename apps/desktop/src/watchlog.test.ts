// 「WatchDog 連線狀況」 without the DOM: the window's own journal of its link
// to the watchdog (one record per change, bounded to 72 hours and 500
// records, working without storage), and its merge with the watchdog's own
// start and stop lines, newest first, with the unclean-stop marker.

import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import type { ErrorReason, EventEntry, ProtocolError } from "./protocol";
import { zoneOf } from "./time";
import {
  guiLabel,
  JOURNAL_KEEP_MS,
  JOURNAL_KEY,
  JOURNAL_MAX,
  LinkJournal,
  type LinkRecord,
  LinkWatch,
  uncleanStop,
  watchdogLabel,
  durationText,
  linkError,
  linkFields,
  linkText,
  pruned,
  watchdogLineView,
  watchdogLines,
} from "./watchlog";

/// 2026-01-01 00:00:00 UTC, 08:00 in Taipei.
const T = 1_767_225_600_000;
const taipei = zoneOf({ configured: null, effective: "Asia/Taipei", utc_offset_secs: 28_800 });

class MemoryStorage {
  readonly items = new Map<string, string>();
  writes = 0;
  getItem(key: string): string | null {
    return this.items.get(key) ?? null;
  }
  setItem(key: string, value: string): void {
    this.writes += 1;
    this.items.set(key, value);
  }
}

const broken = {
  getItem(): string | null {
    throw new Error("SecurityError");
  },
  setItem(): void {
    throw new Error("QuotaExceededError");
  },
};

const LIMIT = 15_000;

/// A window: its journal, its watch, and a clock both run on.
function windowWith(storage: MemoryStorage | typeof broken | null = new MemoryStorage(), startMs = T) {
  const journal = new LinkJournal(storage, startMs);
  const watch = new LinkWatch(journal, () => LIMIT);
  let n = 0;
  let mono = 0;
  let wall = startMs;
  return {
    journal,
    watch,
    /// Advances both clocks.
    pass(ms: number) {
      mono += ms;
      wall += ms;
    },
    /// One snapshot request answered at once: ok, or failed with `error`.
    ok() {
      n += 1;
      watch.sent(n, mono);
      return watch.answered(n, { ok: true, generatedAtMs: wall }, wall);
    },
    fail(error: "not_running" | "refused" | "not_the_watchdog" | "timeout" | "other" = "not_running") {
      n += 1;
      watch.sent(n, mono);
      return watch.answered(n, { ok: false, error }, wall);
    },
    /// A request sent and never answered.
    hang() {
      n += 1;
      watch.sent(n, mono);
    },
    check() {
      return watch.check(mono, wall);
    },
    /// Every second for `secs` seconds, as the window's tick does.
    ticks(secs: number) {
      let added = 0;
      for (let i = 0; i < secs; i += 1) {
        this.pass(1_000);
        if (this.check()) added += 1;
      }
      return added;
    },
    get wall() {
      return wall;
    },
  };
}

const kinds = (records: readonly LinkRecord[]) => records.map((r) => r.event.kind);

describe("the link journal", () => {
  it("records a loss once, however many refreshes fail, and the return with how long it was gone", () => {
    const w = windowWith();
    expect(w.ok()).toBe(false);
    w.pass(5_000);
    const lastOk = w.wall;
    expect(w.ok()).toBe(false);
    w.pass(5_000);
    expect(w.fail("not_running")).toBe(true);
    for (let i = 0; i < 10; i += 1) {
      w.pass(5_000);
      expect(w.fail("not_running")).toBe(false);
      expect(w.fail("timeout")).toBe(false);
      expect(w.check()).toBe(false);
    }
    w.pass(5_000);
    expect(w.ok()).toBe(true);
    w.pass(5_000);
    expect(w.ok()).toBe(false);
    const [restored, lost] = w.journal.list();
    expect(kinds(w.journal.list())).toEqual(["restored", "lost"]);
    expect(lost?.event).toEqual({ kind: "lost", lastOkMs: lastOk, error: "not_running" });
    expect(lost?.atMs).toBe(lastOk + 5_000);
    expect(restored?.event).toEqual({ kind: "restored", goneMs: 55_000 });
  });

  it("records a request left unanswered past the threshold as a loss with no answer", () => {
    const w = windowWith();
    w.ok();
    w.hang();
    expect(w.ticks(15)).toBe(0);
    expect(w.ticks(1)).toBe(1);
    // Still nothing back, and newer requests hang too: no second record.
    w.hang();
    expect(w.ticks(30)).toBe(0);
    expect(w.journal.list()[0]?.event).toEqual({ kind: "lost", lastOkMs: T, error: null });
    // The answer to a newer request settles the older ones.
    expect(w.ok()).toBe(true);
    expect(w.ticks(30)).toBe(0);
    expect(kinds(w.journal.list())).toEqual(["restored", "lost"]);
  });

  it("does not take a suspended window for a lost watchdog", () => {
    const w = windowWith();
    w.ok();
    w.hang();
    w.ticks(3);
    // The window's timers stopped for a minute (sleep, a hidden window).
    w.pass(60_000);
    expect(w.check()).toBe(false);
    expect(w.ok()).toBe(false);
    expect(w.journal.list()).toEqual([]);
  });

  it("records a start that never reaches the watchdog once", () => {
    const w = windowWith();
    expect(w.fail("refused")).toBe(true);
    w.pass(5_000);
    expect(w.fail("refused")).toBe(false);
    expect(w.journal.list().map((r) => r.event)).toEqual([{ kind: "unreachable", error: "refused" }]);
    // A start whose first request hangs is the same, with no answer.
    const h = windowWith();
    h.hang();
    expect(h.ticks(20)).toBe(1);
    expect(h.journal.list().map((r) => r.event)).toEqual([{ kind: "unreachable", error: null }]);
  });

  it("says the watchdog is back when the window opens again after a loss, and only then", () => {
    const storage = new MemoryStorage();
    const first = windowWith(storage);
    first.ok();
    first.pass(5_000);
    first.fail("not_running");
    // Closed while the watchdog was gone; opened again an hour later.
    const second = windowWith(storage, T + 3_600_000);
    expect(second.ok()).toBe(true);
    expect(second.journal.list()[0]?.event).toEqual({ kind: "restored", goneMs: null });
    // Opened again while all is well: nothing to say.
    const third = windowWith(storage, T + 7_200_000);
    expect(third.ok()).toBe(false);
    expect(kinds(third.journal.list())).toEqual(["restored", "lost"]);
  });

  it("writes storage only when the link changes, never on a refresh", () => {
    const storage = new MemoryStorage();
    const w = windowWith(storage);
    for (let i = 0; i < 20; i += 1) {
      w.pass(5_000);
      w.ok();
      w.check();
    }
    expect(storage.writes).toBe(0);
    w.fail();
    w.fail();
    w.ok();
    w.ok();
    expect(storage.writes).toBe(2);
    expect([...storage.items.keys()]).toEqual([JOURNAL_KEY]);
    expect(JOURNAL_KEY).toBe("wgal.watchdogLink");
    // Kinds and times only: no error text, no names, no addresses.
    const stored = JSON.parse(storage.items.get(JOURNAL_KEY) ?? "") as { v: number; records: LinkRecord[] };
    expect(stored.v).toBe(1);
    expect(stored.records.map((r) => Object.keys(r.event).sort())).toEqual([
      ["error", "kind", "lastOkMs"],
      ["goneMs", "kind"],
    ]);
  });

  it("keeps at most 72 hours and 500 records, dropped when a record is written", () => {
    expect(JOURNAL_KEEP_MS).toBe(72 * 3_600_000);
    expect(JOURNAL_MAX).toBe(500);
    const storage = new MemoryStorage();
    const journal = new LinkJournal(storage, T);
    for (let i = 0; i < 600; i += 1) {
      journal.add(i % 2 === 0 ? { kind: "lost", lastOkMs: null, error: null } : { kind: "restored", goneMs: 1_000 }, T + i);
    }
    const kept = journal.list();
    expect(kept).toHaveLength(500);
    expect(kept[0]?.atMs).toBe(T + 599);
    expect(kept[499]?.atMs).toBe(T + 100);
    // 72 hours later a window shows only the last 72 hours; the storage
    // keeps the rest until the next write, which drops them.
    const later = T + 599 + JOURNAL_KEEP_MS + 1;
    const reopened = new LinkJournal(storage, later - 2);
    expect(reopened.list().map((r) => r.atMs)).toEqual([T + 599, T + 598]);
    const writes = storage.writes;
    expect((JSON.parse(storage.items.get(JOURNAL_KEY) ?? "") as { records: unknown[] }).records).toHaveLength(500);
    reopened.add({ kind: "restored", goneMs: null }, later);
    expect(storage.writes).toBe(writes + 1);
    expect(reopened.list().map((r) => r.atMs)).toEqual([later]);
    expect((JSON.parse(storage.items.get(JOURNAL_KEY) ?? "") as { records: unknown[] }).records).toHaveLength(1);
    // A record far in the future (a clock set wrong once) is dropped too.
    expect(pruned([{ id: 1, atMs: T + JOURNAL_KEEP_MS + 1, event: { kind: "restored", goneMs: null } }], T)).toEqual([]);
  });

  it("works for the session when storage is missing, throws or holds something else", () => {
    for (const storage of [null, broken]) {
      const w = windowWith(storage);
      expect(w.journal.persistent).toBe(false);
      w.ok();
      w.fail();
      w.ok();
      expect(kinds(w.journal.list())).toEqual(["restored", "lost"]);
      expect(w.journal.persistent).toBe(false);
    }
    for (const garbage of ["{not json", "42", '{"records":"x"}', '{"records":[{"id":"1"},{"id":1,"atMs":5,"event":{"kind":"lost"}}]}']) {
      const storage = new MemoryStorage();
      storage.items.set(JOURNAL_KEY, garbage);
      const journal = new LinkJournal(storage, T);
      expect(journal.list(), garbage).toEqual([]);
      expect(journal.persistent).toBe(true);
      journal.add({ kind: "restored", goneMs: null }, T);
      expect(new LinkJournal(storage, T).list()).toHaveLength(1);
    }
  });
});

describe("why the window could not reach the watchdog", () => {
  const err = (code: ProtocolError["code"], reason: ErrorReason | null, message = ""): ProtocolError => ({
    code,
    reason,
    message,
  });

  it("tells the kinds apart by the shell's reason, never by its message", () => {
    expect(linkError(err("internal", "not_running", "anything"))).toBe("not_running");
    expect(linkError(err("internal", "not_the_watchdog"))).toBe("not_the_watchdog");
    expect(linkError(err("unauthorized", "caller_not_allowed"))).toBe("refused");
    expect(linkError(err("timeout", "no_answer_in_time"))).toBe("timeout");
    expect(linkError(err("timeout", "too_many_waiting"))).toBe("timeout");
    expect(linkError(err("internal", "too_many_waiting"))).toBe("timeout");
    expect(linkError(err("internal", "cannot_reach_watchdog"))).toBe("other");
    expect(linkError(err("internal", "answer_lost"))).toBe("other");
    // The old texts mean nothing any more.
    expect(linkError(err("internal", null, "the background watchdog service is not installed or not running"))).toBe(
      "other",
    );
    // An answer without a reason is judged by its code.
    expect(linkError(err("unauthorized", null, "denied"))).toBe("refused");
    expect(linkError(err("timeout", null, "late"))).toBe("timeout");
    expect(linkError(err("unsupported_version", null))).toBe("other");
    expect(linkError(null)).toBe("other");
  });

  it("covers every reason the shell gives", () => {
    const ipc = readFileSync(new URL("../src-tauri/src/ipc.rs", import.meta.url), "utf8");
    const reasons = new Set([...ipc.matchAll(/Some\(ErrorReason::(\w+)\)/g)].map((m) => m[1]));
    expect([...reasons].sort()).toEqual([
      "AnswerLost",
      "CallerNotAllowed",
      "CannotReachWatchdog",
      "InvalidAnswer",
      "NoAnswerInTime",
      "NotRunning",
      "NotTheWatchdog",
      "TooManyWaiting",
    ]);
  });

  it("says each record in words and in fields", () => {
    const time = (ms: number) => `@${ms}`;
    expect(linkText({ kind: "lost", lastOkMs: 7, error: "not_running" }, time)).toBe(
      "無法連上 WatchDog：背景監控服務未執行或未安裝（最後一次正常：@7）",
    );
    expect(linkText({ kind: "lost", lastOkMs: null, error: null }, time)).toBe("無法連上 WatchDog：一直沒有收到回應");
    expect(linkText({ kind: "unreachable", error: "refused" }, time)).toBe(
      "介面啟動後無法連上 WatchDog：背景監控拒絕了這個帳號（不在本機安裝的存取清單中）",
    );
    expect(linkText({ kind: "restored", goneMs: 125_000 }, time)).toBe("恢復連線（失聯約 2 分 05 秒）");
    expect(linkText({ kind: "restored", goneMs: null }, time)).toContain("介面關閉期間何時恢復不明");
    expect(linkFields({ kind: "lost", lastOkMs: 7, error: null })).toBe("kind=link_lost error=no_answer last_ok_unix_ms=7");
    expect(linkFields({ kind: "unreachable", error: "timeout" })).toBe("kind=link_unreachable_at_start error=timeout");
    expect(linkFields({ kind: "restored", goneMs: null })).toBe("kind=link_restored gone_ms=unknown");
    expect([5_000, 59_400, 60_000, 3_599_000, 3_600_000, 90_000_000].map(durationText)).toEqual([
      "5 秒",
      "59 秒",
      "1 分 00 秒",
      "59 分 59 秒",
      "1 小時 00 分",
      "1 天 1 小時",
    ]);
  });
});

describe("the merged view", () => {
  let seq = 0;
  const line = (kind: "watchdog_started" | "watchdog_stopped", at: number): EventEntry => {
    seq += 1;
    return { seq, at_unix_ms: at, tunnel_id: null, removed_tunnel: false, event: { kind } };
  };
  const record = (id: number, atMs: number, kind: "lost" | "restored"): LinkRecord => ({
    id,
    atMs,
    event: kind === "lost" ? { kind, lastOkMs: atMs - 5_000, error: "not_running" } : { kind, goneMs: 20_000 },
  });

  it("puts the watchdog's lines and the window's records together, newest first", () => {
    const entries = [line("watchdog_started", T + 30_000), line("watchdog_stopped", T + 5_000), line("watchdog_started", T)];
    const records = [record(2, T + 35_000, "restored"), record(1, T + 10_000, "lost")];
    const merged = watchdogLines(entries, false, records);
    expect(merged.map((l) => (l.source === "gui" ? `gui:${l.record.event.kind}` : `wd:${l.entry.event.kind}`))).toEqual([
      "gui:restored",
      "wd:watchdog_started",
      "gui:lost",
      "wd:watchdog_stopped",
      "wd:watchdog_started",
    ]);
    // At the same moment the window's record comes first.
    const same = watchdogLines([line("watchdog_started", T)], false, [record(3, T, "restored")]);
    expect(same.map((l) => l.source)).toEqual(["gui", "watchdog"]);
    // Keys never clash between the two.
    expect(new Set(merged.map((l) => l.key)).size).toBe(merged.length);
  });

  it("holds back records older than the oldest line shown while older lines are still to load", () => {
    const entries = [line("watchdog_started", T + 60_000), line("watchdog_stopped", T + 50_000)].sort(
      (a, b) => b.at_unix_ms - a.at_unix_ms,
    );
    const records = [record(2, T + 55_000, "restored"), record(1, T + 10_000, "lost")];
    expect(watchdogLines(entries, true, records).filter((l) => l.source === "gui")).toHaveLength(1);
    expect(watchdogLines(entries, false, records).filter((l) => l.source === "gui")).toHaveLength(2);
    // Nothing from the watchdog (none, or it cannot be read): every record.
    expect(watchdogLines([], true, records)).toHaveLength(2);
  });

  it("marks a start that follows another start without a stop", () => {
    const entries = [
      line("watchdog_started", T),
      line("watchdog_started", T + 10_000),
      line("watchdog_stopped", T + 20_000),
      line("watchdog_started", T + 30_000),
      line("watchdog_started", T + 40_000),
    ].reverse();
    const merged = watchdogLines(entries, false, []);
    const unclean = merged.map((l) => (l.source === "watchdog" ? l.unclean : null));
    // Newest first: the start at +40 s follows the one at +30 s; the start
    // at +10 s follows the one at T. The oldest start is never judged.
    expect(unclean).toEqual([true, false, false, true, false]);
    const view = watchdogLineView(merged[0]!, new Map(), taipei, T);
    expect(view.text).toBe(`背景監控已啟動（${uncleanStop()}）`);
    expect(uncleanStop()).toBe("上次未正常停止");
    expect(view.categoryLabel).toBe("系統");
    expect(view.who).toBe(watchdogLabel());
    expect(watchdogLabel()).toBe("背景監控");
  });

  it("shows the window's records as 系統, with their own icons and times in the zone", () => {
    const [lost, restored] = [record(1, T, "lost"), record(2, T + 25_000, "restored")].map((r) =>
      watchdogLineView({ key: `g${r.id}`, source: "gui", record: r }, new Map(), taipei, T),
    );
    expect(lost).toEqual({
      time: "01-01 08:00:00",
      category: "info",
      icon: "⚠",
      categoryLabel: "系統",
      who: guiLabel(),
      text: "無法連上 WatchDog：背景監控服務未執行或未安裝（最後一次正常：01-01 07:59:55）",
      tone: "down",
    });
    expect(restored?.icon).toBe("✓");
    expect(restored?.tone).toBe("restored");
    expect(restored?.text).toBe("恢復連線（失聯約 20 秒）");
    expect(guiLabel()).toBe("介面觀察");
  });
});
