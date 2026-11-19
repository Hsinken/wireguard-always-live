// The log tab's words and paging, without the DOM. Every kind of line in
// the contract must have a sentence and a category: the sample table below
// is typed over the generated `LogEvent`, and its kinds are compared with
// the ones the generated binding lists, so a new kind fails here until it
// has a text.

import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import {
  CATEGORY,
  hostLabel,
  LOG_CATEGORY,
  type LogKind,
  NO_LOG,
  SYSTEM_FILTER_SINCE,
  SYSTEM_OPTION,
  WATCHDOG_OPTION,
  removedLabel,
  entryView,
  eventText,
  logFilterOf,
  logFilterValue,
  olderThan,
  systemFilterOffered,
  systemOffer,
  withNewest,
  withOlder,
} from "./log";
import type { EventEntry, LogEvent, ResponseEnvelope } from "./protocol";
import { t } from "./i18n";
import { zoneOf } from "./time";

/// 2026-01-01 00:00:00 UTC.
const T = 1_767_225_600_000;
const taipei = zoneOf({ configured: null, effective: "Asia/Taipei", utc_offset_secs: 28_800 });

/// One line of every kind. A missing kind is a type error.
const SAMPLES: { [K in LogKind]: Extract<LogEvent, { kind: K }> } = {
  watchdog_started: { kind: "watchdog_started" },
  watchdog_stopped: { kind: "watchdog_stopped" },
  monitoring_paused: { kind: "monitoring_paused" },
  monitoring_resumed: { kind: "monitoring_resumed" },
  tunnel_registered: { kind: "tunnel_registered" },
  tunnel_removed: { kind: "tunnel_removed" },
  settings_changed: { kind: "settings_changed" },
  auto_recovery_changed: { kind: "auto_recovery_changed", enabled: true },
  tunnel_notifications_changed: { kind: "tunnel_notifications_changed", enabled: false },
  user_connect: { kind: "user_connect" },
  user_disconnect: { kind: "user_disconnect" },
  user_reconnect: { kind: "user_reconnect" },
  probe_failed: { kind: "probe_failed", streak: 2, needed: 3 },
  probe_recovered: { kind: "probe_recovered", after: 2 },
  fault_confirmed: { kind: "fault_confirmed", incident_id: "inc-0007" },
  recovery_started: { kind: "recovery_started", attempt: 1 },
  recovery_succeeded: { kind: "recovery_succeeded" },
  recovery_failed: { kind: "recovery_failed", next_retry_unix_ms: T + 60_000 },
  retries_exhausted: { kind: "retries_exhausted", retries: 100 },
  restarted_after_external_stop: { kind: "restarted_after_external_stop" },
  control_failed: { kind: "control_failed", error: "permission_denied" },
  traffic_verified: { kind: "traffic_verified" },
  light_changed: { kind: "light_changed", light: "red", reason: "fault_confirmed" },
  stopped_externally: { kind: "stopped_externally" },
  started_externally: { kind: "started_externally" },
  local_network_offline: { kind: "local_network_offline" },
  local_network_online: { kind: "local_network_online" },
  incident_closed: { kind: "incident_closed", incident_id: "inc-0007" },
  notification_sent: { kind: "notification_sent", incident_id: "inc-0007", notice: "alert" },
  notification_failed: { kind: "notification_failed", incident_id: "inc-0007", failure: "rate_limited", notice: "alert" },
  notification_cancelled: { kind: "notification_cancelled", incident_id: "inc-0007", notice: "alert" },
  test_notification: { kind: "test_notification", state: "accepted" },
  storage_failed: { kind: "storage_failed" },
  storage_recovered: { kind: "storage_recovered" },
  lines_lost: { kind: "lines_lost", count: 3 },
  agent_request: { kind: "agent_request", op: "set_tunnel_monitoring" },
};

/// The kinds the generated binding lists, read from its source.
function generatedKinds(): string[] {
  const source = readFileSync(new URL("./protocol/generated/LogEvent.ts", import.meta.url), "utf8");
  return [...source.matchAll(/\{ "kind": "([a-z_]+)"/g)].map((m) => m[1]!);
}

const at = (seq: number, event: LogEvent = { kind: "probe_failed", streak: 1, needed: 3 }): EventEntry => ({
  seq,
  at_unix_ms: T + seq * 1_000,
  tunnel_id: "office-1",
  removed_tunnel: false,
  event,
});

/// Newest first, as the watchdog answers: seqs `from` down to `to`.
const page = (from: number, to: number) => Array.from({ length: from - to + 1 }, (_, i) => at(from - i));

describe("every kind of line has words", () => {
  it("covers exactly the kinds of the contract", () => {
    const kinds = generatedKinds();
    expect(kinds.length).toBe(36);
    expect(Object.keys(SAMPLES).sort()).toEqual([...kinds].sort());
    expect(Object.keys(LOG_CATEGORY).sort()).toEqual([...kinds].sort());
  });

  it("gives every kind a sentence and a category with a shape and a word", () => {
    const sentences = new Set<string>();
    for (const event of Object.values(SAMPLES)) {
      for (const host of [false, true]) {
        const text = eventText(event, host, () => "08:01:00");
        expect(text, event.kind).not.toBe("");
        expect(text).not.toMatch(/undefined|null|\[object/);
        sentences.add(text);
      }
      const category = CATEGORY[LOG_CATEGORY[event.kind]];
      expect(category.icon).not.toBe("");
      expect(category.label).not.toBe("");
    }
    expect(sentences.size).toBeGreaterThanOrEqual(34);
    // Shapes and words tell the categories apart, not color.
    expect(new Set(Object.values(CATEGORY).map((c) => c.icon)).size).toBe(Object.keys(CATEGORY).length);
    expect(new Set(Object.values(CATEGORY).map((c) => c.label)).size).toBe(Object.keys(CATEGORY).length);
  });

  it("says what the owner asked to see in plain words", () => {
    const text = (event: LogEvent) => eventText(event, false, () => "08:01:00");
    expect(text(SAMPLES.user_disconnect)).toBe("使用者斷線");
    expect(text(SAMPLES.user_connect)).toBe("使用者連線");
    expect(text(SAMPLES.fault_confirmed)).toBe("意外斷線：故障已確認（事件 inc-0007）");
    expect(text(SAMPLES.probe_failed)).toBe("探測失敗（連續 2 / 3）");
    expect(text(SAMPLES.probe_recovered)).toBe("探測恢復（之前連續失敗 2 次）");
    expect(text(SAMPLES.recovery_started)).toBe("自動重連第 1 輪開始");
    expect(text(SAMPLES.recovery_succeeded)).toBe("自動重連成功，通訊已驗證");
    expect(text(SAMPLES.recovery_failed)).toBe("自動重連失敗，下次重試 08:01:00");
    expect(text({ kind: "recovery_failed", next_retry_unix_ms: null })).toBe("自動重連失敗，未排定下次重試");
    expect(text(SAMPLES.retries_exhausted)).toBe("已重試 100 次達到上限，停止自動復原");
    expect(text(SAMPLES.stopped_externally)).toContain("外部停止");
    expect(text(SAMPLES.local_network_offline)).toContain("本機網路離線");
    expect(eventText(SAMPLES.monitoring_paused, true, String)).toBe("已暫停全部監看");
    expect(eventText(SAMPLES.monitoring_paused, false, String)).toBe("已暫停監看這條隧道");
  });

  it("tells manual, automatic and accidental apart", () => {
    const label = (kind: LogKind) => CATEGORY[LOG_CATEGORY[kind]].label;
    expect(label("user_disconnect")).toBe("人工");
    expect(label("user_connect")).toBe("人工");
    expect(label("recovery_started")).toBe("自動");
    expect(label("restarted_after_external_stop")).toBe("自動");
    expect(label("fault_confirmed")).toBe("異常");
    expect(label("probe_failed")).toBe("異常");
    expect(label("stopped_externally")).toBe("外部");
    expect(label("probe_recovered")).toBe("恢復");
  });
});

describe("which tunnel a line is about", () => {
  const names = new Map([["office-1", "辦公室"]]);
  const line = (tunnel_id: string | null, event: LogEvent, removed_tunnel = false): EventEntry => ({
    seq: 1,
    at_unix_ms: T,
    tunnel_id,
    removed_tunnel,
    event,
  });

  it("names a registered tunnel, the host, or a tunnel removed since", () => {
    expect(entryView(line("office-1", SAMPLES.user_connect), names, taipei, T).who).toBe("辦公室");
    expect(entryView(line(null, SAMPLES.watchdog_started), names, taipei, T).who).toBe(hostLabel());
    expect(entryView(line(null, SAMPLES.settings_changed), names, taipei, T).text).toContain("主機設定");
    expect(entryView(line(null, SAMPLES.user_connect, true), names, taipei, T).who).toBe(removedLabel());
    // The watchdog says which: a removed tunnel's pause is not the host's.
    expect(entryView(line(null, SAMPLES.monitoring_paused, true), names, taipei, T).who).toBe(removedLabel());
    expect(entryView(line(null, SAMPLES.monitoring_paused), names, taipei, T).who).toBe(hostLabel());
    // Not in the snapshot (none yet): its ID.
    expect(entryView(line("lab-2", SAMPLES.user_connect), names, taipei, T).who).toBe("lab-2");
  });

  it("writes the time in the configured zone, without this year", () => {
    const view = entryView(line("office-1", SAMPLES.recovery_failed), names, taipei, T);
    expect(view.time).toBe("01-01 08:00:00");
    expect(view.text).toBe("自動重連失敗，下次重試 01-01 08:01:00");
    // Seen from the next year, the year is written.
    expect(entryView(line("office-1", SAMPLES.user_connect), names, taipei, T + 366 * 86_400_000).time).toBe(
      "2026-01-01 08:00:00",
    );
    expect(view).toMatchObject({ category: "fault", icon: "✕", categoryLabel: "異常" });
  });

  it("reads the contract's sample page", () => {
    const fixture = JSON.parse(
      readFileSync(new URL("../../../crates/wgal-protocol/fixtures/response_event_log.json", import.meta.url), "utf8"),
    ) as ResponseEnvelope;
    if (!("ok" in fixture.outcome) || fixture.outcome.ok.kind !== "event_log") throw new Error("not an event log");
    for (const entry of fixture.outcome.ok.entries) {
      const view = entryView(entry, new Map(), taipei, T);
      expect(view.text, entry.event.kind).not.toBe("");
      expect(view.who).not.toBe("");
    }
  });
});

describe("paging newest first", () => {
  it("loads the newest page, then older pages below it", () => {
    let state = withNewest(NO_LOG, page(300, 201), true);
    expect(state.entries.map((e) => e.seq).slice(0, 2)).toEqual([300, 299]);
    expect(state.olderExist).toBe(true);
    expect(olderThan(state)).toBe(201);
    state = withOlder(state, page(200, 101), true);
    state = withOlder(state, page(100, 1), false);
    expect(state.entries).toHaveLength(300);
    expect(state.olderExist).toBe(false);
    expect(olderThan(state)).toBe(1);
  });

  it("adds new lines on top and keeps what is shown", () => {
    let state = withNewest(NO_LOG, page(300, 201), true);
    state = withOlder(state, page(200, 101), true);
    state = withNewest(state, page(303, 204), true);
    expect(state.entries.map((e) => e.seq).slice(0, 4)).toEqual([303, 302, 301, 300]);
    expect(state.entries).toHaveLength(203);
    // Older pages are still there, and still more below.
    expect(olderThan(state)).toBe(101);
    expect(state.olderExist).toBe(true);
  });

  it("starts over when lines may be missing in between, or the log was replaced", () => {
    let state = withNewest(NO_LOG, page(300, 201), true);
    // A whole page newer than everything shown, and more below it: a gap.
    state = withNewest(state, page(450, 351), true);
    expect(state.entries.map((e) => e.seq)[0]).toBe(450);
    expect(state.entries).toHaveLength(100);
    expect(olderThan(state)).toBe(351);
    // Older than what is shown: the watchdog's log is not the one shown.
    state = withNewest(state, page(10, 1), false);
    expect(state.entries).toHaveLength(10);
    expect(state.olderExist).toBe(false);
    // Empty: nothing to show, and nothing older.
    state = withNewest(state, [], false);
    expect(state).toEqual({ entries: [], olderExist: false, empty: false });
  });

  it("drops lines it already has from an older page", () => {
    let state = withNewest(NO_LOG, page(300, 201), true);
    state = withOlder(state, page(205, 106), true);
    expect(state.entries.map((e) => e.seq).filter((s) => s >= 201 && s <= 205)).toEqual([205, 204, 203, 202, 201]);
    expect(state.entries).toHaveLength(195);
  });
});

describe("the filter's choices (系統事件, owner request of 2026-11-04)", () => {
  it("stands for every filter of the contract and back", () => {
    const filters = [
      { kind: "all" },
      { kind: "system" },
      { kind: "watchdog" },
      { kind: "tunnel", tunnel_id: "home-3" },
    ] as const;
    for (const filter of filters) expect(logFilterOf(logFilterValue(filter))).toEqual(filter);
    expect(logFilterValue({ kind: "system" })).toBe(SYSTEM_OPTION);
    expect(logFilterValue({ kind: "watchdog" })).toBe(WATCHDOG_OPTION);
    // Neither is a tunnel ID, which is only letters, digits, - and _.
    for (const value of [SYSTEM_OPTION, WATCHDOG_OPTION]) expect(value).not.toMatch(/^[A-Za-z0-9_-]+$/);
  });

  it("offers the system lines only from the watchdog version that reads them apart", () => {
    expect(SYSTEM_FILTER_SINCE).toBe("1.1.0");
    expect(systemFilterOffered("1.1.0")).toBe(true);
    expect(systemFilterOffered("1.2.3")).toBe(true);
    expect(systemFilterOffered("2.0.0")).toBe(true);
    // The installed 1.0.2 refuses { kind: "system" }.
    expect(systemFilterOffered("1.0.2")).toBe(false);
    expect(systemFilterOffered(null)).toBe(false);
    // Codex P3-N11: only a plain major.minor.patch counts.
    for (const odd of ["1.1.garbage", "2.invalid", "1.1", "1.1.0-rc1", " 1.1.0", "1.1.0 ", "", "v1.1.0", "1.1.0.1"]) {
      expect(systemFilterOffered(odd), odd).toBe(false);
    }
  });

  it("is named in the language file", () => {
    expect(t("log.filter_system")).toBe("系統事件");
  });

  it("holds a refusal only for the version the refused request was sent to (Codex P3-N16)", () => {
    // Sent to 1.1.0, refused; the answer arrives while the snapshot already
    // names 1.2.0: the refusal is recorded against 1.1.0, so 1.2.0 is offered.
    const refusedBy = "1.1.0";
    expect(systemOffer("1.2.0", refusedBy)).toEqual({ offered: true, refusedBy: null });
    // The same version as the refusal: not offered, and still remembered.
    expect(systemOffer("1.1.0", refusedBy)).toEqual({ offered: false, refusedBy: "1.1.0" });
    // Lost (no version): not offered, the refusal kept for when it is back.
    expect(systemOffer(null, refusedBy)).toEqual({ offered: false, refusedBy: "1.1.0" });
    // A refusal sent while no version was known holds until one is.
    expect(systemOffer(null, "")).toEqual({ offered: false, refusedBy: "" });
    expect(systemOffer("1.1.0", "")).toEqual({ offered: true, refusedBy: null });
    // No refusal: the version alone decides.
    expect(systemOffer("1.1.0", null)).toEqual({ offered: true, refusedBy: null });
    expect(systemOffer("1.0.2", null)).toEqual({ offered: false, refusedBy: null });
  });
});
