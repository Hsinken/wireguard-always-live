// The window with many tunnels: 1, 5 and 16 registered (the contract's
// MAX_TUNNELS), each with 4 observe targets (MAX_OBSERVE_TARGETS), display
// names near the 96-byte DisplayName limit, mixed lights and reasons, and a
// discovery of 32 (MAX_DISCOVERED). View model only: no DOM, no Tauri.

import { describe, expect, it } from "vitest";

import {
  DEFAULT_PROBE_TIMING,
  MAX_DISCOVERED,
  MAX_OBSERVE_TARGETS,
  MAX_TUNNELS,
  type DiscoveredTunnel,
  type Light,
  type LightReason,
  type ProbeResult,
  type Snapshot,
  type TargetStatus,
  type TunnelStatus,
} from "./protocol";
import { LIGHT } from "./strings";
import {
  type Link,
  type RegisterAnswer,
  LOST_AFTER_MS,
  storageFailedClause,
  discoveryView,
  isCurrent,
  tray,
  viewModel,
} from "./view";

const T = 1_767_225_600_000;
/// The default timing's freshness (`ProbeTiming::freshness_secs`), which
/// every tunnel below reports unless it says otherwise.
const FRESH_SECS = 35;
const PROBE_FRESH_MS = FRESH_SECS * 1000;
const DISPLAY_NAME_MAX_BYTES = 96;
const bytes = (s: string) => Buffer.byteLength(s, "utf8");

/// Names at or just under the limit: CJK (3 bytes a character) for even
/// rows, one unbroken ASCII word for odd ones, which is the hardest to wrap.
function longName(i: number): string {
  const n = String(i).padStart(2, "0");
  return i % 2 === 0 ? `${"長".repeat(31)}${n}` : `${"Office-VPN-".repeat(9).slice(0, 94)}${n}`;
}

/// A result that finished `age` ms before the snapshot.
function probe(result: ProbeResult | null, age = 1_500): TargetStatus["last_probe"] {
  return result === null
    ? null
    : { started_at_unix_ms: T - age - 500, finished_at_unix_ms: T - age, result };
}

/// Ten minutes: what a result looks like once the watchdog stopped probing.
const OLD = 600_000;

/// Four observe targets: one ICMP reply, one TCP refusal (an answer), one
/// with no reply and one not probed yet. Two of them fail when `down`; all
/// three results are `age` ms old.
function observe(i: number, down: boolean, age = 1_500): TargetStatus[] {
  const host = (k: number) => `198.51.100.${i * 4 + k + 1}`;
  return [
    { target: { address: host(0), probe: { kind: "icmp" } }, last_probe: probe({ result: "reply", rtt_ms: 9 }, age) },
    {
      target: { address: host(1), probe: { kind: "tcp_connect", port: 65535 } },
      last_probe: probe(down ? { result: "error", error: "permission_denied" } : { result: "refused", rtt_ms: 3 }, age),
    },
    {
      target: { address: host(2), probe: { kind: "icmp" } },
      last_probe: probe(down ? { result: "no_reply" } : { result: "reply", rtt_ms: 40 }, age),
    },
    { target: { address: host(3), probe: { kind: "tcp_connect", port: 443 } }, last_probe: null },
  ];
}

interface Kind {
  light: Light;
  reason: LightReason;
  over?: Partial<TunnelStatus>;
  /// Its observe targets have failing results.
  down?: boolean;
  /// Its observe results are old: the watchdog does not probe it now (down,
  /// disconnected, monitoring off, unreadable, or being stopped, started or
  /// verified), so they are from before.
  old?: boolean;
}

/// Sixteen different rows, in an order where the first five already mix
/// every light.
const KINDS: Kind[] = [
  { light: "green", reason: "verified", down: true },
  { light: "red", reason: "recovery_failed", over: { next_retry_unix_ms: T + 120_000, data_health: "failed", recovery: "backoff" } },
  { light: "yellow", reason: "verifying", down: true, old: true, over: { deadline_unix_ms: T + 45_000, recovery: "verifying" } },
  {
    light: "gray",
    reason: "disconnected_by_user",
    down: true,
    old: true,
    over: { os_state: "disconnected", intent: { desired: "disconnected", monitoring_enabled: true, auto_recovery_enabled: true, start_when_stopped_externally: false } },
  },
  { light: "red", reason: "fault_confirmed", down: true, over: { data_health: "failed" } },
  { light: "yellow", reason: "confirming", over: { deadline_unix_ms: T + 30_000, data_health: "suspect" } },
  { light: "gray", reason: "not_connected", down: true, old: true, over: { os_state: "disconnected" } },
  { light: "red", reason: "binding_lost", down: true, old: true, over: { takeover: "binding_lost", os_state: "unknown" } },
  {
    light: "gray",
    reason: "monitoring_off",
    down: true,
    old: true,
    over: { intent: { desired: "connected", monitoring_enabled: false, auto_recovery_enabled: false, start_when_stopped_externally: false } },
  },
  {
    light: "red",
    reason: "externally_started",
    down: true,
    old: true,
    over: {
      takeover: "externally_controlled",
      intent: { desired: "disconnected", monitoring_enabled: true, auto_recovery_enabled: false, start_when_stopped_externally: false },
    },
  },
  { light: "yellow", reason: "grace_period", old: true, over: { deadline_unix_ms: T + 30_000, os_state: "unknown", data_health: "unknown" } },
  { light: "red", reason: "status_unknown", old: true, over: { os_state: "unknown", data_health: "unknown" } },
  { light: "red", reason: "stopped_externally", old: true, over: { takeover: "stopped_externally", os_state: "disconnected" } },
  { light: "gray", reason: "not_configured", over: { recovery_target: null } },
  { light: "red", reason: "path_unverified", down: true, over: { auto_recovery_allowed: false } },
  { light: "yellow", reason: "reconnecting", down: true, old: true, over: { deadline_unix_ms: T + 75_000, recovery: "starting" } },
];

function tunnel(i: number): TunnelStatus {
  const kind = KINDS[i % KINDS.length]!;
  const base: TunnelStatus = {
    tunnel_id: i === 3 ? "x".repeat(62) + String(i).padStart(2, "0") : `tunnel-${String(i).padStart(2, "0")}`,
    display_name: longName(i),
    light: kind.light,
    light_reason: kind.reason,
    light_since_unix_ms: T - 60_000,
    deadline_unix_ms: null,
    next_retry_unix_ms: null,
    last_verified_unix_ms: kind.light === "green" ? T - 1_500 : null,
    intent: { desired: "connected", monitoring_enabled: true, auto_recovery_enabled: i % 3 === 0, start_when_stopped_externally: false },
    auto_recovery_allowed: true,
    notifications_enabled: i % 4 !== 1,
    takeover: "managed",
    os_state: "connected",
    data_health: "healthy",
    recovery: "idle",
    recovery_target: {
      target: { address: `192.0.2.${i + 1}`, probe: i % 2 === 0 ? { kind: "icmp" } : { kind: "tcp_connect", port: 22 } },
      last_probe: probe(kind.light === "red" ? { result: "no_reply" } : { result: "reply", rtt_ms: 12 }),
    },
    observe_targets: observe(i, kind.down ?? false, kind.old ? OLD : 1_500),
    probe: { ...DEFAULT_PROBE_TIMING },
    freshness_secs: FRESH_SECS,
    consecutive_failures: kind.light === "red" ? 4 : kind.reason === "confirming" ? 2 : 0,
    fault:
      kind.light === "red"
        ? { kind: "route_mismatch", message: "route evidence did not match the bound interface; ".repeat(5).slice(0, 256) }
        : null,
    incident:
      kind.reason === "recovery_failed"
        ? {
            incident_id: `inc-${"0".repeat(56)}${String(i).padStart(4, "0")}`,
            opened_at_unix_ms: T - 600_000,
            first_recovery: "failed",
            backoff_step: 3,
            stable_since_unix_ms: null,
            notification: "unknown",
            retries_used: 3,
            retries_exhausted: false,
            exhausted_notification: "not_eligible",
          }
        : null,
    retry: { limit: 100, interval_secs: 60 },
    recovered_notification: null,
    auto_recovery_pending: false,
  };
  return { ...base, ...kind.over };
}

function snapshot(tunnels: TunnelStatus[], over: Partial<Snapshot> = {}): Snapshot {
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
      workers: { configured: null, suggested: 4, limit: MAX_TUNNELS + 1, effective: 4, running: 2 },
      time_zone: { configured: "Asia/Taipei", effective: "Asia/Taipei", utc_offset_secs: 28_800 },
    },
    notification: { enabled: false, device_name: "", chat_id: null, token: "not_set", last_test: null, last_failure: null, templates_id: null, cli_language: null, host_name: null },
    tunnels,
    ...over,
  };
}

const many = (n: number) => Array.from({ length: n }, (_, i) => tunnel(i));
const fresh = (s: Snapshot): Link => ({ snapshot: s, receivedAtMono: 1_000, lastError: null });

/// What the tray must count, from the fixture's own description rather than
/// the code under test: red lights, and the two failing observe targets of
/// every row whose failures are current.
function expected(n: number) {
  const kinds = Array.from({ length: n }, (_, i) => KINDS[i % KINDS.length]!);
  return {
    red: kinds.filter((k) => k.light === "red").length,
    host: kinds.filter((k) => k.down && !k.old).length * 2,
  };
}

describe("the fixtures are what the contract allows at most", () => {
  it("has names near the DisplayName limit and four observe targets a tunnel", () => {
    for (const t of many(MAX_TUNNELS)) {
      expect(bytes(t.display_name)).toBeLessThanOrEqual(DISPLAY_NAME_MAX_BYTES);
      expect(bytes(t.display_name)).toBeGreaterThanOrEqual(DISPLAY_NAME_MAX_BYTES - 4);
      expect(t.tunnel_id.length).toBeLessThanOrEqual(64);
      expect(t.observe_targets).toHaveLength(MAX_OBSERVE_TARGETS);
      expect(bytes(t.fault?.message ?? "")).toBeLessThanOrEqual(256);
    }
    expect(new Set(many(MAX_TUNNELS).map((t) => t.light_reason)).size).toBe(MAX_TUNNELS);
  });
});

describe.each([1, 5, MAX_TUNNELS])("%i tunnels", (n) => {
  const s = snapshot(many(n));
  const vm = viewModel(fresh(s), 1_000);

  it("shows every tunnel once, in the watchdog's order, with its whole name", () => {
    expect(vm.rows.map((r) => r.id)).toEqual(s.tunnels.map((t) => t.tunnel_id));
    expect(new Set(vm.rows.map((r) => r.label)).size).toBe(n);
    vm.rows.forEach((row, i) => {
      const t = s.tunnels[i]!;
      // Never cut: the layout wraps it, so no tooltip is needed to read it.
      expect(row.name).toBe(t.display_name);
      expect(row.label).toBe(t.display_name);
      expect(row.light).toBe(t.light);
      expect(row.observe).toHaveLength(MAX_OBSERVE_TARGETS);
      expect(row.details[0]).toEqual(["隧道 ID", t.tunnel_id]);
      expect(row.updatedMs).toBe(T);
    });
  });

  it("counts VPN faults and current host problems for the tray, apart", () => {
    const want = expected(n);
    expect(vm.tray.vpnFaults).toBe(want.red);
    expect(vm.tray.hostIssues).toBe(want.host);
    expect(vm.rows.reduce((sum, r) => sum + r.hostIssues, 0)).toBe(want.host);
    expect(vm.tray.level).toBe(want.red > 0 ? "fault" : want.host > 0 ? "host" : "ok");
    expect(vm.tray.monitor).toBe("ok");
    expect(vm.tray.tooltip).toContain(`VPN 異常 ${want.red}`);
    expect(vm.tray.tooltip).toContain(`主機目標異常 ${want.host}`);
  });

  it("keeps the tray tooltip and the header line short, whatever the count", () => {
    // Counts only, never names: the length does not grow with the list.
    expect(vm.tray.tooltip.length).toBeLessThanOrEqual(60);
    expect(vm.summary.length).toBeLessThanOrEqual(70);
    expect(vm.summary.startsWith(`${n} 條隧道：`)).toBe(true);
    const counted = [...vm.summary.matchAll(/[●▲■○] \S+? (\d+)/g)].reduce((sum, m) => sum + Number(m[1]), 0);
    expect(counted).toBe(n);
    for (const t of s.tunnels) expect(vm.summary).not.toContain(t.display_name);
  });

  it("says each row's observe targets in one line, old results as old", () => {
    vm.rows.forEach((row, i) => {
      const kind = KINDS[i % KINDS.length]!;
      expect(row.observeSummary, row.id).toBe(
        kind.old
          ? "僅監看目標：4 個（目前沒有新結果）"
          : `僅監看目標：4 個，${kind.down ? 2 : 0} 個異常，1 個沒有新結果`,
      );
      expect(row.hostIssues).toBe(kind.down && !kind.old ? 2 : 0);
      for (const o of row.observe.slice(0, 3)) {
        expect(o.current).toBe(!kind.old);
        expect(o.note).toBe(kind.old ? "上次結果，已超過 35 秒" : "");
      }
      // Never probed: not current, and nothing to say about its age.
      expect(row.observe[3]).toMatchObject({ current: false, atMs: null, note: "", result: "尚未探測" });
    });
  });

  it("drops every light and count when the watchdog is lost", () => {
    const lost = viewModel({ snapshot: s, receivedAtMono: 0, lastError: null }, LOST_AFTER_MS + 1);
    expect(lost.rows).toHaveLength(n);
    for (const row of lost.rows) {
      expect(row.light).toBe("unknown");
      expect(row.hostIssues).toBe(0);
      expect(row.observe.every((o) => !o.current)).toBe(true);
      expect(row.observe.filter((o) => o.atMs !== null).every((o) => o.note === "上次結果")).toBe(true);
      expect(row.edit).toEqual({ kind: "disabled", why: "背景監控失聯" });
    }
    expect(lost.tray).toMatchObject({ level: "lost", vpnFaults: 0, hostIssues: 0, monitor: "lost" });
    expect(lost.summary).toBe(`${n} 條隧道：? 狀態未知（背景監控失聯）`);
  });

  it("counts no old observe result while monitoring is paused", () => {
    // Paused for longer than the freshness window: nothing was probed since.
    const paused = snapshot(
      many(n).map((t, i) => ({
        ...t,
        light: "gray" as const,
        light_reason: "monitoring_off" as const,
        observe_targets: observe(i, true, PROBE_FRESH_MS + 1),
      })),
      { monitoring_paused: true },
    );
    const summary = tray(paused, false, T);
    expect(summary.hostIssues).toBe(0);
    expect(summary.level).toBe("paused");
    expect(viewModel(fresh(paused), 1_000).summary).toBe(`${n} 條隧道：${LIGHT.gray.icon} ${LIGHT.gray.label} ${n}；監看已暫停`);
  });

  it("offers the target editor while the watchdog answers", () => {
    for (const row of vm.rows) expect(row.edit).toEqual({ kind: "enabled" });
  });
});

describe("rows that look alike", () => {
  it("adds the tunnel ID to names reported twice", () => {
    const a = { ...tunnel(0), display_name: "Office" };
    const b = { ...tunnel(1), display_name: "Office" };
    const c = { ...tunnel(2), display_name: "Lab" };
    const rows = viewModel(fresh(snapshot([a, b, c])), 1_000).rows;
    expect(rows.map((r) => r.label)).toEqual(["Office（tunnel-00）", "Office（tunnel-01）", "Lab"]);
  });
});

describe("observe results judged by their age (the tunnel's freshness_secs)", () => {
  const failingAt = (age: number): TargetStatus[] => [
    { target: { address: "198.51.100.1", probe: { kind: "icmp" } }, last_probe: probe({ result: "no_reply" }, age) },
  ];

  it("are current up to freshness_secs before the snapshot, inclusive, and not after it", () => {
    expect(isCurrent(T, T, FRESH_SECS)).toBe(true);
    expect(isCurrent(T - PROBE_FRESH_MS, T, FRESH_SECS)).toBe(true);
    expect(isCurrent(T - PROBE_FRESH_MS - 1, T, FRESH_SECS)).toBe(false);
    expect(isCurrent(T - 51_000, T, 51)).toBe(true);
    expect(isCurrent(T - 51_001, T, 51)).toBe(false);
    expect(isCurrent(null, T, FRESH_SECS)).toBe(false);
    // Stamped after the snapshot: the watchdog's clock moved back.
    expect(isCurrent(T + 1, T, FRESH_SECS)).toBe(false);
  });

  it("use each tunnel's own freshness, not a fixed 35 s", () => {
    // 40 s old: stale with the default timing, current for a tunnel probed
    // every 30 s with a 3 s timeout (51 s).
    const slow = {
      ...tunnel(0),
      probe: { interval_secs: 30, timeout_secs: 3, retries: 1, min_fault_secs: 90 },
      freshness_secs: 51,
      observe_targets: failingAt(40_000),
    };
    const fast = { ...tunnel(1), observe_targets: failingAt(40_000) };
    const vm = viewModel(fresh(snapshot([slow, fast])), 1_000);
    expect(vm.rows[0]!.hostIssues).toBe(1);
    expect(vm.rows[1]!.hostIssues).toBe(0);
    expect(vm.rows[1]!.observe[0]!.note).toBe("上次結果，已超過 35 秒");
    expect(vm.tray.hostIssues).toBe(1);
  });

  it("count a failure as a host problem only while current, at the boundary too", () => {
    const count = (age: number) => {
      const vm = viewModel(fresh(snapshot([{ ...tunnel(0), observe_targets: failingAt(age) }])), 1_000);
      return [vm.tray.hostIssues, vm.rows[0]!.hostIssues, vm.rows[0]!.observeSummary] as const;
    };
    expect(count(PROBE_FRESH_MS)).toEqual([1, 1, "僅監看目標：1 個，1 個異常"]);
    expect(count(PROBE_FRESH_MS + 1)).toEqual([0, 0, "僅監看目標：1 個（目前沒有新結果）"]);
  });

  it("do not count an old failure while the tunnel is stopped, started or verified", () => {
    for (const recovery of ["queued", "stopping", "starting", "verifying"] as const) {
      const t = { ...tunnel(0), recovery, light: "yellow" as const, light_reason: "reconnecting" as const };
      const old = viewModel(fresh(snapshot([{ ...t, observe_targets: failingAt(OLD) }])), 1_000);
      expect(old.tray.hostIssues, recovery).toBe(0);
      expect(old.tray.level, recovery).toBe("ok");
      expect(old.rows[0]!.observeSummary).toBe("僅監看目標：1 個（目前沒有新結果）");
      expect(old.rows[0]!.observe[0]).toMatchObject({ failing: true, current: false, note: "上次結果，已超過 35 秒" });
      // A failure seen 5 s ago is news whatever the tunnel does now.
      const recent = viewModel(fresh(snapshot([{ ...t, observe_targets: failingAt(5_000) }])), 1_000);
      expect(recent.tray.hostIssues, recovery).toBe(1);
    }
  });

  it("keep their time, so an old result reads as old", () => {
    const row = viewModel(fresh(snapshot([tunnel(3)])), 1_000).rows[0]!;
    expect(row.observe.map((o) => o.atMs)).toEqual([T - OLD, T - OLD, T - OLD, null]);
    expect(row.observe.filter((o) => o.failing)).toHaveLength(2);
    expect(row.hostIssues).toBe(0);
  });
});

describe("the header line and a storage failure", () => {
  it("colours each count as its light, and what is wrong red (owner request of 2026-11-04)", () => {
    const s = snapshot([tunnel(0), tunnel(15)].map((t) => ({ ...t, light: "green" as const, light_reason: "verified" as const })), {
      storage: "failed",
    });
    const vm = viewModel(fresh(s), 1_000);
    expect(vm.summaryParts).toEqual([
      { text: "2 條隧道：", tone: null },
      { text: `${LIGHT.green.icon} ${LIGHT.green.label} 2`, tone: "green" },
      { text: "；", tone: null },
      { text: "主機目標異常 2", tone: "red" },
      { text: "；", tone: null },
      { text: storageFailedClause(), tone: "red" },
    ]);
    expect(vm.summaryParts.map((p) => p.text).join("")).toBe(vm.summary);
    const lights = (["red", "yellow", "green", "gray"] as const).map((light) => ({
      ...tunnel(0),
      tunnel_id: `t-${light}`,
      light,
      light_reason: light === "green" ? ("verified" as const) : light === "red" ? ("fault_confirmed" as const) : light === "yellow" ? ("confirming" as const) : ("not_connected" as const),
    }));
    const mixed = viewModel(fresh(snapshot(lights)), 1_000).summaryParts.filter((p) => p.tone !== null);
    expect(mixed.map((p) => p.tone)).toEqual(["red", "yellow", "green", "gray", "red"]);
    expect(mixed[0]!.text).toBe(`${LIGHT.red.icon} ${LIGHT.red.label} 1`);
  });

  it("says what the banner says, and keeps saying it when the banner has scrolled away", () => {
    const s = snapshot([tunnel(0), tunnel(15)].map((t) => ({ ...t, light: "green" as const, light_reason: "verified" as const })), {
      storage: "failed",
    });
    const vm = viewModel(fresh(s), 1_000);
    expect(vm.summary).toBe(`2 條隧道：${LIGHT.green.icon} ${LIGHT.green.label} 2；主機目標異常 2；${storageFailedClause()}`);
    expect(vm.banner).toContain("已停止自動復原與通知");
    expect(vm.tray.monitor).toBe("storage_failed");
  });

  it("names both a storage failure and a pause, and works without tunnels", () => {
    const both = viewModel(fresh(snapshot([], { storage: "failed", monitoring_paused: true })), 1_000);
    expect(both.summary).toBe(`尚未登記任何隧道；${storageFailedClause()}；監看已暫停`);
    expect(viewModel(fresh(snapshot([])), 1_000).summary).toBe("尚未登記任何隧道");
  });

  it("stays short with 16 tunnels and every clause", () => {
    const vm = viewModel(fresh(snapshot(many(MAX_TUNNELS), { storage: "failed", monitoring_paused: true })), 1_000);
    expect(vm.summary).toContain(storageFailedClause());
    expect(vm.summary.endsWith("監看已暫停")).toBe(true);
    expect(vm.summary.length).toBeLessThanOrEqual(100);
  });
});

describe(`discovery of ${MAX_DISCOVERED} tunnels`, () => {
  const found: DiscoveredTunnel[] = Array.from({ length: MAX_DISCOVERED }, (_, i) => ({
    // The first 16 are the registered ones of `many`.
    tunnel_id: i < MAX_TUNNELS ? tunnel(i).tunnel_id : `tunnel-${String(i).padStart(2, "0")}`,
    display_name: longName(i),
    control: i % 7 === 6 ? "unsupported" : i % 5 === 4 ? "unknown" : "supported",
  }));
  const none = new Map<string, RegisterAnswer>();

  it("lists every one with its name and ID, and says the list may be cut", () => {
    const view = discoveryView(found, snapshot(many(5)), none, false);
    expect(view.entries).toHaveLength(MAX_DISCOVERED);
    view.entries.forEach((e, i) => {
      expect(e.label).toBe(`${found[i]!.display_name}（${found[i]!.tunnel_id}）`);
    });
    expect(view.note).toContain(`${MAX_DISCOVERED}`);
    expect(discoveryView(found.slice(0, 31), null, none, false).note).toBeNull();
    expect(discoveryView(found.slice(0, 31), null, none, false, true).note).toBeNull();
  });

  it("says where the official WireGuard app is when a discovery finds nothing (owner request of 2026-11-05)", () => {
    // Before any discovery, nothing is said.
    expect(discoveryView([], null, none, false).note).toBeNull();
    const view = discoveryView([], snapshot(many(5)), none, false, true);
    expect(view.entries).toEqual([]);
    expect(view.note).toBe(
      "找不到官方 WireGuard 的隧道。若尚未安裝，請先安裝官方 WireGuard 程式（https://www.wireguard.com/install/）；已安裝的話，請先在官方程式中加入並啟用要監看的隧道，再按「新增隧道」。",
    );
  });

  it("never claims a tunnel is on the access list before the watchdog answers", () => {
    const view = discoveryView(found, snapshot(many(5)), none, false);
    for (const e of view.entries.slice(5)) {
      expect(e.status.text, e.id).toBe("");
      if (e.register.kind === "disabled") expect(e.register.why).not.toContain("清單");
      if (e.unsupported) expect(e.register.kind).toBe("disabled");
      else expect(e.register.kind, e.id).toBe("enabled");
    }
    for (const e of view.entries.slice(0, 5)) {
      expect(e.status.text).toBe("已登記");
      expect(e.register).toEqual({ kind: "disabled", why: "已登記" });
    }
  });

  it("shows the watchdog's own answer next to the tunnel it is about, its original text in the details", () => {
    const refusal = "this tunnel is not on the installation's access list; an administrator must do this";
    const answers = new Map<string, RegisterAnswer>([
      ["tunnel-20", { kind: "pending" }],
      ["tunnel-21", { kind: "refused", words: () => ({ text: "需要管理員：…", detail: `unauthorized: ${refusal}` }) }],
      ["tunnel-22", { kind: "accepted" }],
      ["tunnel-23", { kind: "no_answer", words: () => ({ text: "無法連到背景監控；是否已登記以接下來的狀態為準", detail: "x" }) }],
    ]);
    const view = discoveryView(found, snapshot(many(5)), answers, false);
    const by = (id: string) => view.entries.find((e) => e.id === id)!;
    expect(by("tunnel-20").register.kind).toBe("disabled");
    expect(by("tunnel-20").status.text).toContain("等待背景監控回覆");
    expect(by("tunnel-21").status).toEqual({ text: "背景監控回覆：需要管理員：…", detail: `unauthorized: ${refusal}` });
    expect(by("tunnel-21").register.kind).toBe("enabled");
    expect(by("tunnel-22").status).toEqual({ text: "背景監控已接受登記", detail: null });
    expect(by("tunnel-23").status.text).toContain("以接下來的狀態為準");
    expect(by("tunnel-23").status.detail).toBe("x");
    expect(by("tunnel-24").status).toEqual({ text: "", detail: null });
  });

  it("stops new registrations at 16 but still takes a lost binding back", () => {
    const registered = many(MAX_TUNNELS);
    const view = discoveryView(found, snapshot(registered), none, false);
    for (const e of view.entries.slice(MAX_TUNNELS)) {
      expect(e.register.kind, e.id).toBe("disabled");
    }
    expect(view.entries.slice(MAX_TUNNELS).filter((e) => !e.unsupported)[0]?.register).toEqual({
      kind: "disabled",
      why: `已登記 ${MAX_TUNNELS} 條，已達上限`,
    });
    // tunnel(7) has lost its binding.
    const relink = view.entries[7]!;
    expect(registered[7]!.takeover).toBe("binding_lost");
    expect(relink.action).toBe("重新登記");
    expect(relink.register.kind).toBe("enabled");
  });

  it("offers nothing while the watchdog is lost", () => {
    const view = discoveryView(found, snapshot(many(5)), none, true);
    for (const e of view.entries) expect(e.register).toEqual({ kind: "disabled", why: "背景監控失聯" });
  });
});

describe("the tray dot with sixteen tunnels", () => {
  it("is red while any monitored tunnel is red, and only then", () => {
    expect(viewModel(fresh(snapshot(many(MAX_TUNNELS))), 1_000).tray.dot).toBe("red");
    // The same list without its red and yellow tunnels: every monitored one
    // left is green or gray.
    const calm = many(MAX_TUNNELS).filter((t) => t.light === "green" || t.light === "gray");
    expect(calm.length).toBeGreaterThan(0);
    expect(viewModel(fresh(snapshot(calm)), 1_000).tray.dot).toBe("green");
    // One tunnel still confirming takes the dot away.
    const confirming = many(MAX_TUNNELS).find((t) => t.light_reason === "confirming")!;
    expect(viewModel(fresh(snapshot([...calm, confirming])), 1_000).tray.dot).toBe("none");
    // Red tunnels whose monitoring is paused do not count.
    const pausedRed = many(MAX_TUNNELS)
      .filter((t) => t.light === "red")
      .map((t) => ({ ...t, intent: { ...t.intent, monitoring_enabled: false } }));
    expect(viewModel(fresh(snapshot([...calm, ...pausedRed])), 1_000).tray.dot).toBe("green");
  });

  it("is gone while everything is paused or the watchdog is lost", () => {
    const s = snapshot(many(MAX_TUNNELS));
    expect(viewModel(fresh({ ...s, monitoring_paused: true }), 1_000).tray.dot).toBe("none");
    expect(viewModel({ snapshot: s, receivedAtMono: 0, lastError: null }, LOST_AFTER_MS + 1).tray.dot).toBe("none");
  });
});

describe("sixteen cards' switches and progress", () => {
  it("carries each tunnel's switches and failing streak", () => {
    const s = snapshot(many(MAX_TUNNELS));
    const rows = viewModel(fresh(s), 1_000).rows;
    rows.forEach((row, i) => {
      const t = s.tunnels[i]!;
      expect(row.monitoring.on).toBe(t.intent.monitoring_enabled);
      expect(row.notifications.on).toBe(t.notifications_enabled);
      expect(row.judgment).toBe("斷線後約 62～77 秒判定故障（連續 3 次失敗，且至少 60 秒）");
      const probed = t.intent.monitoring_enabled && t.consecutive_failures > 0;
      expect(row.failures !== null).toBe(probed);
    });
  });
});
