// The settings tab and the new form checks, without the DOM: worker
// threads, time zone, probe timing and observe targets, against the
// contract's ranges. The watchdog checks everything again.

import { describe, expect, it } from "vitest";

import { parseObserveTargets, parseTimeZoneName, parseTiming, parseWorkers } from "./forms";
import { hostView, mcpTelegramView, parseHostSettings } from "./host";
import { DEFAULT_PROBE_TIMING, MAX_WORKERS, MIN_WORKERS, type Snapshot, type TunnelStatus } from "./protocol";
import { restoreTab } from "./tabname";

const T = 1_767_225_600_000;

function tunnel(id: string, monitoring = true): TunnelStatus {
  return {
    tunnel_id: id,
    display_name: id,
    light: "green",
    light_reason: "verified",
    light_since_unix_ms: T,
    deadline_unix_ms: null,
    next_retry_unix_ms: null,
    last_verified_unix_ms: T,
    intent: { desired: "connected", monitoring_enabled: monitoring, auto_recovery_enabled: false, start_when_stopped_externally: false },
    auto_recovery_allowed: false,
    notifications_enabled: true,
    takeover: "observe_only",
    os_state: "connected",
    data_health: "healthy",
    recovery: "idle",
    recovery_target: null,
    observe_targets: [],
    probe: { ...DEFAULT_PROBE_TIMING },
    freshness_secs: 35,
    consecutive_failures: 0,
    fault: null,
    incident: null,
    retry: { limit: 100, interval_secs: 60 },
    recovered_notification: null,
    auto_recovery_pending: false,
  };
}

function snapshot(over: Partial<Snapshot> = {}, tunnels = [tunnel("a"), tunnel("b"), tunnel("c", false)]): Snapshot {
  return {
    generated_at_unix_ms: T,
    storage: "ok",
    capabilities: {
      query_state: "supported",
      start_stop: "supported",
      bound_icmp_probe: "supported",
      bound_tcp_probe: "unknown",
      read_metrics: "unsupported",
      verified_concurrent_tunnels: 1,
      background_operation: "unknown",
    },
    monitoring_paused: false,
    host: {
      watchdog_version: "1.0.0",
      min_gui_version: "1.0.0",
      min_usable_gui_version: "1.0.0",
      mcp_enabled: false,
      mcp_telegram_enabled: false,
      workers: { configured: null, suggested: 3, limit: 3, effective: 3, running: 1 },
      time_zone: { configured: null, effective: "Asia/Taipei", utc_offset_secs: 28_800 },
    },
    notification: { enabled: false, device_name: "", chat_id: null, token: "not_set", last_test: null, last_failure: null, templates_id: null, cli_language: null, host_name: null },
    tunnels,
    ...over,
  };
}

describe("worker threads", () => {
  it("shows the suggestion, the limit, what is used and what runs", () => {
    const view = hostView(snapshot(), false, "Asia/Taipei")!;
    expect(view.workers).toMatchObject({ auto: true, count: "3", autoLabel: "自動（建議 3）", min: MIN_WORKERS, max: 3 });
    expect(view.workers.facts).toEqual([
      ["建議", "3"],
      ["上限", "3（監看中 2 條 + 1，至少 2）"],
      ["目前最多使用", "3"],
      ["執行中", "1（只在需要時啟動）"],
    ]);
    expect(view.save).toEqual({ kind: "enabled" });
    expect(hostView(snapshot(), true, null)!.save.kind).toBe("disabled");
    expect(hostView(null, false, null)).toBeNull();
  });

  it("counts no tunnel as monitored while everything is paused", () => {
    const view = hostView(snapshot({ monitoring_paused: true }), false, null)!;
    expect(view.workers.facts[1]![1]).toContain("監看中 0 條");
  });

  it("accepts 2 up to the limit, or the count already configured", () => {
    expect(parseWorkers("2", 3, null)).toEqual({ ok: true, value: 2 });
    expect(parseWorkers("3", 3, null)).toEqual({ ok: true, value: 3 });
    expect(parseWorkers("4", 3, null).ok).toBe(false);
    expect(parseWorkers("1", 3, null).ok).toBe(false);
    expect(parseWorkers("", 3, null).ok).toBe(false);
    expect(parseWorkers("2.5", 3, null).ok).toBe(false);
    // Configured at 6 before tunnels were paused: saving the zone keeps it.
    expect(parseWorkers("6", 3, 6)).toEqual({ ok: true, value: 6 });
    expect(parseWorkers(String(MAX_WORKERS + 1), MAX_WORKERS, null).ok).toBe(false);
    // With nothing monitored the limit is still 2.
    expect(parseWorkers("2", 1, null)).toEqual({ ok: true, value: 2 });
  });
});

describe("time zone", () => {
  it("says what following the system means now", () => {
    expect(hostView(snapshot(), false, "Asia/Taipei")!.zone).toEqual({
      follow: true,
      name: "Asia/Taipei",
      followLabel: "跟隨系統（目前 Asia/Taipei（UTC+08:00））",
      current: "紀錄與 Telegram 訊息目前使用：Asia/Taipei（UTC+08:00）",
    });
    const set = snapshot({
      host: {
        watchdog_version: "1.0.0",
        min_gui_version: "1.0.0",
        min_usable_gui_version: "1.0.0",
        mcp_enabled: false,
        mcp_telegram_enabled: false,
        workers: { configured: 4, suggested: 3, limit: 3, effective: 3, running: 1 },
        time_zone: { configured: "Europe/Berlin", effective: "Europe/Berlin", utc_offset_secs: 3_600 },
      },
    });
    const view = hostView(set, false, "Asia/Taipei")!;
    expect(view.zone).toMatchObject({ follow: false, name: "Europe/Berlin", followLabel: "跟隨系統（這台電腦為 Asia/Taipei）" });
    expect(view.workers).toMatchObject({ auto: false, count: "4", max: 4 });
  });

  it("accepts IANA-form names only, never a path", () => {
    for (const ok of ["UTC", "Asia/Taipei", "Etc/GMT+8", "America/Argentina/Buenos_Aires", "America/Port-au-Prince"]) {
      expect(parseTimeZoneName(ok), ok).toEqual({ ok: true, value: ok });
    }
    for (const bad of ["", "/etc/passwd", "../x", "Asia//Taipei", "Asia/", "8Asia", "Asia Taipei", "C:\\x", "a".repeat(65)]) {
      expect(parseTimeZoneName(bad).ok, bad).toBe(false);
    }
  });

  it("reads the form into HostSettings", () => {
    const s = snapshot();
    expect(parseHostSettings({ workersAuto: true, workers: "x", zoneFollow: true, zone: "x" }, s)).toEqual({
      ok: true,
      value: { workers: null, time_zone: null },
    });
    expect(parseHostSettings({ workersAuto: false, workers: "2", zoneFollow: false, zone: " UTC " }, s)).toEqual({
      ok: true,
      value: { workers: 2, time_zone: "UTC" },
    });
    expect(parseHostSettings({ workersAuto: false, workers: "9", zoneFollow: true, zone: "" }, s).ok).toBe(false);
    expect(parseHostSettings({ workersAuto: true, workers: "", zoneFollow: false, zone: "/x" }, s).ok).toBe(false);
  });
});

describe("probe timing and targets typed on a card", () => {
  it("accepts each field within its range", () => {
    expect(parseTiming({ interval: "15", timeout: "2", retries: "2", minFault: "60" })).toEqual({
      ok: true,
      value: DEFAULT_PROBE_TIMING,
    });
    expect(parseTiming({ interval: "10", timeout: "1", retries: "0", minFault: "0" }).ok).toBe(true);
    expect(parseTiming({ interval: "3600", timeout: "10", retries: "10", minFault: "3600" }).ok).toBe(true);
  });

  it("names the field that is out of range", () => {
    const base = { interval: "15", timeout: "2", retries: "2", minFault: "60" };
    const error = (over: Partial<typeof base>) => {
      const parsed = parseTiming({ ...base, ...over });
      return parsed.ok ? "" : parsed.error;
    };
    expect(error({ interval: "9" })).toContain("探測間隔");
    expect(error({ interval: "3601" })).toContain("探測間隔");
    expect(error({ timeout: "0" })).toContain("逾時");
    expect(error({ timeout: "11" })).toContain("逾時");
    expect(error({ retries: "11" })).toContain("重試次數");
    expect(error({ minFault: "3601" })).toContain("最短故障時間");
    expect(error({ interval: "" })).toContain("整數");
    expect(error({ retries: "-1" })).toContain("整數");
    expect(error({ minFault: "1e3" })).toContain("整數");
  });

  it("reads observe targets one per line, at most four", () => {
    expect(parseObserveTargets("198.51.100.1\n\n 198.51.100.2:445 \r\n")).toEqual({
      ok: true,
      value: [
        { address: "198.51.100.1", probe: { kind: "icmp" } },
        { address: "198.51.100.2", probe: { kind: "tcp_connect", port: 445 } },
      ],
    });
    expect(parseObserveTargets("")).toEqual({ ok: true, value: [] });
    expect(parseObserveTargets("1.1.1.1\n1.1.1.2\n1.1.1.3\n1.1.1.4\n1.1.1.5").ok).toBe(false);
    const bad = parseObserveTargets("198.51.100.1\nexample.com");
    expect(bad.ok ? "" : bad.error).toContain("example.com");
    expect(parseObserveTargets("fe80::1").ok).toBe(false);
  });
});

describe("the remembered tab", () => {
  it("opens on the tunnels unless a known tab is remembered", () => {
    expect(restoreTab(null)).toBe("tunnels");
    expect(restoreTab("log")).toBe("log");
    expect(restoreTab("settings")).toBe("settings");
    expect(restoreTab("nonsense")).toBe("tunnels");
    expect(restoreTab("")).toBe("tunnels");
  });
});

describe("letting agents set Telegram", () => {
  const host = (watchdog_version: string, mcp_enabled: boolean, mcp_telegram_enabled: boolean) =>
    snapshot({ host: { ...snapshot().host, watchdog_version, mcp_enabled, mcp_telegram_enabled } });

  it("is offered only by a watchdog from 2.1.0 that is reachable", () => {
    for (const view of [mcpTelegramView(host("1.9.9", true, false), false), mcpTelegramView(null, false), mcpTelegramView(host("2.1.0", true, false), true)]) {
      expect(view.state).toBe("需要 WatchDog 2.1.0 以上，且背景監控正常連線");
      expect(view.control).toEqual({ kind: "disabled", why: "需要 WatchDog 2.1.0 以上，且背景監控正常連線" });
    }
  });

  it("is off by default and turned the other way while MCP is on", () => {
    expect(mcpTelegramView(host("2.1.0", true, false), false)).toEqual({
      state: "未允許（預設）：Agent 讀不到 Chat ID，也不能更換這些設定",
      toggle: "允許",
      control: { kind: "enabled" },
    });
    expect(mcpTelegramView(host("2.1.0", true, true), false)).toEqual({
      state: "已允許：Agent 可以讀取 Chat ID，並更換或清除裝置顯示名稱、Chat ID 與 Bot Token",
      toggle: "取消允許",
      control: { kind: "enabled" },
    });
  });

  it("keeps its setting, greyed out and without effect, while MCP is off", () => {
    expect(mcpTelegramView(host("2.1.0", false, true), false)).toEqual({
      state: "已允許：Agent 可以讀取 Chat ID，並更換或清除裝置顯示名稱、Chat ID 與 Bot Token。MCP 關閉時不生效，開啟 MCP 後恢復",
      toggle: "取消允許",
      control: { kind: "disabled", why: "先開啟 MCP 才能變更" },
    });
  });
});
