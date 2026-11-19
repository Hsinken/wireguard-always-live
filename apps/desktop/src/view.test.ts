// The view model and its texts, against snapshots built from the generated
// contract types. No DOM, no Tauri, no watchdog.

import { describe, expect, it } from "vitest";

import {
  fitDeviceName,
  looksLikeBotToken,
  newRequestId,
  parseAddress,
  parseChatId,
  parseRetry,
  parseTarget,
} from "./forms";
import {
  DEFAULT_PROBE_TIMING,
  DEFAULT_RECOVERY_RETRY,
  type LightReason,
  type ProtocolError,
  type Request,
  type Snapshot,
  type TunnelStatus,
} from "./protocol";
import { noAnswer, snapshotRefusal } from "./errors";
import { t } from "./i18n";
import { lostAfterMs } from "./prefs";
import { plain } from "./said";
import { LIGHT, LIGHT_REASON } from "./strings";
import {
  ExpiryWatch,
  LOST_AFTER_MS,
  LatestOnly,
  TRAY_GREEN_MAX_MS,
  arrivalAgeMs,
  countdown,
  expiredTunnels,
  isLost,
  notificationView,
  messageDeviceName,
  resultText,
  tray,
  trayCommand,
  trayDot,
  viewModel,
  watchdogNowMs,
  type Link,
} from "./view";
import { blankName } from "./templates";

const T = 1_767_225_600_000;


function tunnel(over: Partial<TunnelStatus> = {}): TunnelStatus {
  return {
    tunnel_id: "office-1",
    display_name: "Office",
    light: "green",
    light_reason: "verified",
    light_since_unix_ms: T - 60_000,
    deadline_unix_ms: null,
    next_retry_unix_ms: null,
    last_verified_unix_ms: T - 1_000,
    intent: {
      desired: "connected",
      monitoring_enabled: true,
      auto_recovery_enabled: true,
      start_when_stopped_externally: false,
    },
    auto_recovery_allowed: true,
    notifications_enabled: true,
    takeover: "managed",
    os_state: "connected",
    data_health: "healthy",
    recovery: "idle",
    recovery_target: {
      target: { address: "192.0.2.1", probe: { kind: "icmp" } },
      last_probe: {
        started_at_unix_ms: T - 1_020,
        finished_at_unix_ms: T - 1_000,
        result: { result: "reply", rtt_ms: 12 },
      },
    },
    observe_targets: [],
    probe: { ...DEFAULT_PROBE_TIMING },
    freshness_secs: 35,
    consecutive_failures: 0,
    fault: null,
    incident: null,
    retry: { ...DEFAULT_RECOVERY_RETRY },
    recovered_notification: null,
    auto_recovery_pending: false,
    ...over,
  };
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
      workers: { configured: null, suggested: 2, limit: 2, effective: 2, running: 1 },
      time_zone: { configured: null, effective: "Asia/Taipei", utc_offset_secs: 28_800 },
    },
    notification: {
      enabled: false,
      device_name: "",
      chat_id: null,
      token: "not_set",
      last_test: null,
      last_failure: null,
      templates_id: null,
      cli_language: null,
      host_name: null,
    },
    tunnels,
    ...over,
  };
}

const fresh = (s: Snapshot): Link => ({ snapshot: s, receivedAtMono: 1_000, lastError: null });

describe("lights and reasons (F-U1, F-U4)", () => {
  it("has a word and a shape for every light, never color alone", () => {
    for (const light of ["green", "yellow", "red", "gray"] as const) {
      expect(LIGHT[light].icon).not.toBe("");
      expect(LIGHT[light].label).not.toBe("");
    }
    expect(new Set(Object.values(LIGHT).map((l) => l.icon)).size).toBe(4);
  });

  it("has a text for every reason the watchdog can give", () => {
    const reasons = Object.keys(LIGHT_REASON) as LightReason[];
    expect(reasons).toHaveLength(23);
    for (const reason of reasons) {
      const vm = viewModel(fresh(snapshot([tunnel({ light_reason: reason })])), 1_000);
      expect(vm.rows[0]?.reason).toBe(LIGHT_REASON[reason]);
    }
  });

  it("shows the watchdog's light as given, never a derived one", () => {
    const vm = viewModel(
      fresh(snapshot([tunnel({ light: "red", light_reason: "status_unknown", os_state: "unknown", data_health: "unknown" })])),
      1_000,
    );
    expect(vm.rows[0]?.light).toBe("red");
    expect(vm.rows[0]?.details.find(([k]) => k === "OS 狀態代碼")?.[1]).toBe("unknown");
    expect(vm.rows[0]?.osState).toBe("未知");
  });

  it("never shows a failed probe as 0 ms", () => {
    expect(resultText({ result: "no_reply" })).toBe("無回應");
    expect(resultText({ result: "error", error: "permission_denied" })).toContain("permission_denied");
    expect(resultText({ result: "reply", rtt_ms: 0 })).toBe("0 ms");
  });

  it("warns when storage has failed", () => {
    const vm = viewModel(fresh(snapshot([tunnel()], { storage: "failed" })), 1_000);
    expect(vm.banner).toContain("無法保存");
  });
});

describe("lost link (F-U3, T27)", () => {
  it("is lost without a snapshot, after an IPC error, or after 15 s of silence", () => {
    const s = snapshot([tunnel()]);
    expect(isLost({ snapshot: null, receivedAtMono: null, lastError: null }, 0)).toBe(true);
    expect(isLost({ snapshot: s, receivedAtMono: 0, lastError: () => plain("gone") }, 1)).toBe(true);
    expect(isLost({ snapshot: s, receivedAtMono: 0, lastError: null }, LOST_AFTER_MS)).toBe(false);
    expect(isLost({ snapshot: s, receivedAtMono: 0, lastError: null }, LOST_AFTER_MS + 1)).toBe(true);
  });

  it("drops every light, keeps no old green, and disables the buttons", () => {
    const link: Link = { snapshot: snapshot([tunnel()]), receivedAtMono: 0, lastError: null };
    const vm = viewModel(link, LOST_AFTER_MS + 1);
    expect(vm.overlay?.title).toBe("背景監控失聯");
    const row = vm.rows[0];
    expect(row?.light).toBe("unknown");
    expect(row?.lightLabel).toBe("狀態未知");
    expect(row?.deadlineMs).toBeNull();
    expect(row?.connect.kind).toBe("disabled");
    expect(row?.reconnect.kind).toBe("disabled");
    expect(vm.tray.level).toBe("lost");
    expect(vm.tray.monitor).toBe("lost");
  });
});

describe("watchdog errors in the lost-link overlay (plan §3.2, §9.2)", () => {
  const unauthorized: ProtocolError = {
    code: "unauthorized",
    reason: "caller_not_allowed",
    message: "caller is not on the access list",
  };

  it("explains an unauthorized snapshot by its reason, the original text in the details", () => {
    const link: Link = { snapshot: null, receivedAtMono: null, lastError: () => snapshotRefusal(unauthorized) };
    const vm = viewModel(link, 0);
    expect(vm.overlay?.title).toBe("背景監控失聯");
    expect(vm.overlay?.text).toContain(t("error_reason.caller_not_allowed"));
    expect(vm.overlay?.text).not.toContain(unauthorized.message);
    expect(vm.overlay?.detail).toBe("unauthorized (caller_not_allowed): caller is not on the access list");
    expect(vm.tray.level).toBe("lost");
  });

  it("keeps showing an unreachable watchdog as lost, with the shell's reason in words", () => {
    const notRunning: ProtocolError = {
      code: "internal",
      reason: "not_running",
      message: "the background watchdog service is not installed or not running",
    };
    const link: Link = { snapshot: null, receivedAtMono: null, lastError: () => snapshotRefusal(notRunning) };
    const vm = viewModel(link, 0);
    expect(vm.overlay?.text).toContain("背景監控服務未執行或未安裝");
    expect(vm.overlay?.text).not.toContain(notRunning.message);
    expect(vm.overlay?.detail).toContain(notRunning.message);
    expect(vm.tray.monitor).toBe("lost");
  });

  it("says a request that threw in words, what it threw in the details", () => {
    const link: Link = { snapshot: null, receivedAtMono: null, lastError: () => noAnswer(new Error("pipe broke")) };
    const vm = viewModel(link, 0);
    expect(vm.overlay?.text).toContain("無法連到背景監控");
    expect(vm.overlay?.detail).toBe("pipe broke");
    // Silent rather than refused: no details at all.
    const silent = viewModel({ snapshot: null, receivedAtMono: null, lastError: null }, 0);
    expect(silent.overlay?.detail).toBeNull();
  });

  it("uses only answers newer than the last one used, when snapshot requests overlap", () => {
    const order = new LatestOnly();
    const slow = order.issue();
    const fast = order.issue();
    expect(order.accept(fast)).toBe(true);
    // The slow request's late snapshot or late timeout is older news.
    expect(order.accept(slow)).toBe(false);
    expect(order.accept(fast)).toBe(false);
    expect(order.accept(order.issue())).toBe(true);
  });
});

describe("deadlines and retries (F-U2, F-U13)", () => {
  it("counts down on the watchdog's clock plus monotonic time", () => {
    expect(countdown(T + 65_000, T, 0)).toBe("剩 1 分 05 秒");
    expect(countdown(T + 65_000, T, 60_000)).toBe("剩 5 秒");
    expect(countdown(T + 5_000, T, 5_000)).toBe("等待更新");
    expect(countdown(T + 5_000, T, 90_000)).toBe("等待更新");
  });

  it("passes the deadline of a yellow light and the retry of a red one", () => {
    const yellow = tunnel({ light: "yellow", light_reason: "verifying", deadline_unix_ms: T + 45_000 });
    const red = tunnel({ tunnel_id: "lab-2", light: "red", light_reason: "recovery_failed", next_retry_unix_ms: T + 60_000 });
    const vm = viewModel(fresh(snapshot([yellow, red])), 1_000);
    expect(vm.rows[0]?.deadlineMs).toBe(T + 45_000);
    expect(vm.rows[1]?.nextRetryMs).toBe(T + 60_000);
  });
});

describe("tray summary (F-U5, plan §8)", () => {
  it("counts a host target that is down apart from VPN faults", () => {
    const host = tunnel({
      observe_targets: [
        {
          target: { address: "192.0.2.10", probe: { kind: "tcp_connect", port: 445 } },
          last_probe: { started_at_unix_ms: T, finished_at_unix_ms: T, result: { result: "no_reply" } },
        },
      ],
    });
    const summary = tray(snapshot([host]), false, T);
    expect(summary.vpnFaults).toBe(0);
    expect(summary.hostIssues).toBe(1);
    expect(summary.level).toBe("host");
    expect(summary.tooltip).toContain("VPN 異常 0");
  });

  it("puts a VPN fault above host issues, and shows a pause", () => {
    const red = tunnel({ light: "red", light_reason: "fault_confirmed" });
    expect(tray(snapshot([red]), false, T).level).toBe("fault");
    expect(tray(snapshot([tunnel()], { monitoring_paused: true }), false, T).level).toBe("paused");
    expect(tray(snapshot([tunnel()]), false, T).level).toBe("ok");
  });
});

describe("buttons and toggles (F-U6)", () => {
  it("disables control of a tunnel whose binding is lost", () => {
    const lost = tunnel({ light: "red", light_reason: "binding_lost", takeover: "binding_lost" });
    const row = viewModel(fresh(snapshot([lost])), 1_000).rows[0];
    expect(row?.connect.kind).toBe("disabled");
    expect(row?.disconnect.kind).toBe("disabled");
  });

  it("explains why auto recovery cannot be turned on yet", () => {
    const fresh_ = tunnel({
      intent: { desired: "connected", monitoring_enabled: true, auto_recovery_enabled: false, start_when_stopped_externally: false },
      auto_recovery_allowed: false,
    });
    const row = viewModel(fresh(snapshot([fresh_])), 1_000).rows[0];
    expect(row?.autoRecovery.toggle).toEqual({
      kind: "disabled",
      why: "復原判定目標尚未通過初次檢查，或這條隧道無法控制",
    });
  });

  it("offers no toggles while the watchdog is lost", () => {
    const link: Link = { snapshot: snapshot([tunnel()]), receivedAtMono: 0, lastError: null };
    const row = viewModel(link, LOST_AFTER_MS + 1).rows[0];
    expect(row?.autoRecovery.toggle.kind).toBe("disabled");
    expect(row?.startWhenStoppedExternally.toggle.kind).toBe("disabled");
  });

  it("says a paused row is paused, not disconnected, and its OS state is old", () => {
    const paused = tunnel({ light: "gray", light_reason: "monitoring_off" });
    const row = viewModel(fresh(snapshot([paused], { monitoring_paused: true })), 1_000).rows[0];
    expect(row?.reason).toContain("監看已暫停");
    expect(row?.details.find(([k]) => k === "OS 狀態代碼")?.[1]).toContain("暫停期間未更新");
    expect(row?.osState).toBe("已連線（暫停前）");
  });

  it("does not offer to probe while monitoring is paused", () => {
    const row = viewModel(fresh(snapshot([tunnel()], { monitoring_paused: true })), 1_000).rows[0];
    expect(row?.probeNow.kind).toBe("disabled");
    expect(viewModel(fresh(snapshot([tunnel()], { monitoring_paused: true })), 1_000).banner).toContain("暫停");
  });
});

describe("targets (F-U9)", () => {
  it("accepts IPv4 literals the contract accepts", () => {
    expect(parseAddress("192.0.2.1")).toEqual({ ok: true, value: "192.0.2.1" });
    expect(parseAddress(" 10.0.0.1 ")).toEqual({ ok: true, value: "10.0.0.1" });
    expect(parseTarget("192.0.2.1", "")).toEqual({
      ok: true,
      value: { address: "192.0.2.1", probe: { kind: "icmp" } },
    });
    expect(parseTarget("192.0.2.1", "443")).toEqual({
      ok: true,
      value: { address: "192.0.2.1", probe: { kind: "tcp_connect", port: 443 } },
    });
  });

  it("rejects what cannot be a host behind a tunnel, names and IPv6", () => {
    for (const bad of [
      "0.0.0.0",
      "127.0.0.1",
      "224.0.0.1",
      "239.255.255.255",
      "255.255.255.255",
      "169.254.1.1",
      "example.com",
      "fe80::1",
      "::1",
      "192.0.2",
      "192.0.2.256",
      "192.0.02.1",
      "",
    ]) {
      expect(parseAddress(bad).ok, bad).toBe(false);
    }
    expect(parseAddress("fe80::1")).toMatchObject({ ok: false, error: expect.stringContaining("IPv6") });
    for (const port of ["0", "65536", "-1", "80a", "01"]) {
      expect(parseTarget("192.0.2.1", port).ok, port).toBe(false);
    }
  });
});

describe("notification settings (F-U10, F-U11, plan §7.1, §7.4)", () => {
  it("keeps chat IDs as strings", () => {
    expect(parseChatId("-1001234567890")).toEqual({ ok: true, value: "-1001234567890" });
    expect(parseChatId("@my_channel")).toEqual({ ok: true, value: "@my_channel" });
    // Telegram's lengths, 5 either way (owner decision of 2026-11-06).
    for (const good of ["@abcd", "@a", `@${"a".repeat(37)}`, "-9999999999999999999", "7"]) {
      expect(parseChatId(good).ok, good).toBe(true);
    }
    for (const bad of ["", "0", "-0", "007", "-0123", "12 34", "1e5", "@", "@1abcde", `@${"a".repeat(38)}`, "-99999999999999999999", "@<script>"]) {
      expect(parseChatId(bad).ok, bad).toBe(false);
    }
  });

  it("takes any device name, cut to 64 characters with ...", () => {
    for (const name of ["", "<b>Office</b>", "👨‍👩‍👧", "❤️", "می‌خواهم", "x".repeat(64)]) {
      expect(fitDeviceName(name)).toBe(name);
    }
    expect(fitDeviceName("測".repeat(70))).toBe(`${"測".repeat(61)}...`);
    expect(Array.from(fitDeviceName("😀".repeat(70))).length).toBe(64);
    expect(fitDeviceName("Office\nPC")).toBe("Office PC");
    // Counted as Rust counts: a trailing U+FEFF is a 65th character.
    expect(fitDeviceName(`${"x".repeat(64)}\uFEFF`)).toBe(`${"x".repeat(61)}...`);
    expect(fitDeviceName("  Office  ")).toBe("  Office  ");
  });

  it("checks the token format without keeping it", () => {
    expect(looksLikeBotToken("123456789:EXAMPLE-not-a-real-token_0000000000")).toBe(true);
    for (const [id, secret] of [[3, 30], [15, 40], [10, 35]] as const) {
      expect(looksLikeBotToken(`${"9".repeat(id)}:${"a".repeat(secret)}`), `${id}/${secret}`).toBe(true);
    }
    for (const [id, secret] of [[2, 35], [16, 35], [10, 29], [10, 41]] as const) {
      expect(looksLikeBotToken(`${"9".repeat(id)}:${"a".repeat(secret)}`), `${id}/${secret}`).toBe(false);
    }
    expect(looksLikeBotToken("123456789")).toBe(false);
    expect(looksLikeBotToken("abc:EXAMPLE-not-a-real-token_0000000000")).toBe(false);
  });

  it("shows only whether a token is stored, and the test result", () => {
    const s = snapshot([], {
      notification: {
        enabled: true,
        device_name: "辦公室筆電",
        chat_id: "-1001234567890",
        token: "stored",
        last_test: {
          request_id: "gui-1",
          state: "unknown",
          requested_at_unix_ms: T,
          finished_at_unix_ms: T + 1_000,
          failure: "outcome_unknown",
        },
        last_failure: "credentials_rejected",
        templates_id: null,
        cli_language: null,
        host_name: null,
      },
    });
    const n = notificationView(s);
    expect(n.deviceHost).toBe("");
    expect(n.token).toBe("已儲存");
    expect(n.test).toContain("結果不明");
    expect(n.testBusy).toBe(false);
    expect(n.lastFailure).toContain("拒絕");
    const sending = notificationView(
      snapshot([], {
        notification: { ...s.notification, last_test: { ...s.notification.last_test!, state: "sending", finished_at_unix_ms: null, failure: null } },
      }),
    );
    expect(sending.testBusy).toBe(true);
  });

  it("claims nothing about the token without a snapshot", () => {
    expect(notificationView(null).token).toBe("狀態未知");
    expect(notificationView(null).deviceHost).toBe("");
  });

  it("shows the host name messages give while no device name is set", () => {
    const base = snapshot([]);
    const with_ = (device_name: string, host_name: string | null) =>
      snapshot([], { notification: { ...base.notification, device_name, host_name } });
    // Empty, or nothing but spaces: not set, as the watchdog judges it.
    for (const blank of ["", "   "]) {
      expect(notificationView(with_(blank, "office-pc")).deviceHost).toBe("未設定時使用主機名稱：office-pc");
      expect(messageDeviceName(with_(blank, "office-pc").notification)).toBe("office-pc");
      expect(notificationView(with_(blank, null)).deviceHost).toBe("");
      expect(messageDeviceName(with_(blank, null).notification)).toBe("");
    }
    // A set name, spaces around it included, is used as it is.
    expect(notificationView(with_(" Lab ", "office-pc")).deviceHost).toBe("");
    expect(messageDeviceName(with_(" Lab ", "office-pc").notification)).toBe(" Lab ");
    // Blank as Rust's trim judges it: U+FEFF, which JavaScript's trim drops,
    // is a name; U+3000 and U+00A0 are white space to both.
    expect(notificationView(with_("\uFEFF", "office-pc")).deviceHost).toBe("");
    expect(messageDeviceName(with_("\uFEFF", "office-pc").notification)).toBe("\uFEFF");
    expect(messageDeviceName(with_("\u3000\u00a0\t", "office-pc").notification)).toBe("office-pc");
  });

  it("judges a device name blank exactly as Rust's trim does", () => {
    // Every character Rust's char::is_whitespace accepts, and some it does not.
    const rust = [0x9, 0xa, 0xb, 0xc, 0xd, 0x20, 0x85, 0xa0, 0x1680, 0x2028, 0x2029, 0x202f, 0x205f, 0x3000];
    for (let c = 0x2000; c <= 0x200a; c++) rust.push(c);
    for (const c of rust) expect(blankName(String.fromCodePoint(c)), c.toString(16)).toBe(true);
    for (const c of [0xfeff, 0x200b, 0x180e, 0x2060, 0x41]) {
      expect(blankName(String.fromCodePoint(c)), c.toString(16)).toBe(false);
    }
    expect(blankName("")).toBe(true);
    expect(blankName(" a ")).toBe(false);
    expect(messageDeviceName(undefined)).toBe("");
  });

  it("makes a new request ID for every action", () => {
    const a = newRequestId(T);
    const b = newRequestId(T);
    expect(a).not.toBe(b);
    expect(a).toMatch(/^[A-Za-z0-9_-]{1,64}$/);
  });
});

describe("tray dot (owner's request 12)", () => {
  const green = (id: string, over: Partial<TunnelStatus> = {}) => tunnel({ tunnel_id: id, ...over });
  const off = { desired: "connected", monitoring_enabled: false, auto_recovery_enabled: false, start_when_stopped_externally: false } as const;
  const byUser = { desired: "disconnected", monitoring_enabled: true, auto_recovery_enabled: true, start_when_stopped_externally: false } as const;

  it("is green when every monitored tunnel the user wants connected is green", () => {
    expect(trayDot(snapshot([green("a"), green("b")]), false, T)).toBe("green");
    // Disconnected on purpose, and gray: left out.
    const intended = green("b", { light: "gray", light_reason: "disconnected_by_user", intent: byUser, os_state: "disconnected" });
    expect(trayDot(snapshot([green("a"), intended]), false, T)).toBe("green");
    // Paused for one tunnel: not monitored, so its light does not count.
    const paused = green("c", { light: "red", light_reason: "binding_lost", intent: off });
    expect(trayDot(snapshot([green("a"), paused]), false, T)).toBe("green");
  });

  it("is not green when every monitored tunnel is gray", () => {
    const intended = green("b", { light: "gray", light_reason: "disconnected_by_user", intent: byUser, os_state: "disconnected" });
    const unset = green("c", { light: "gray", light_reason: "not_configured", recovery_target: null });
    expect(trayDot(snapshot([intended]), false, T)).toBe("none");
    expect(trayDot(snapshot([intended, unset]), false, T)).toBe("none");
    expect(trayDot(snapshot([intended, unset, green("a")]), false, T)).toBe("green");
  });

  it("is red when any monitored tunnel has a red light, whatever else", () => {
    const red = green("b", { light: "red", light_reason: "fault_confirmed" });
    const yellow = green("c", { light: "yellow", light_reason: "confirming", deadline_unix_ms: T + 30_000 });
    expect(trayDot(snapshot([green("a"), red, yellow]), false, T)).toBe("red");
    expect(tray(snapshot([red]), false, T).dot).toBe("red");
  });

  it("shows no dot while starting or confirming, with nothing monitored, paused or lost", () => {
    const yellow = green("c", { light: "yellow", light_reason: "starting", deadline_unix_ms: T + 30_000 });
    expect(trayDot(snapshot([green("a"), yellow]), false, T)).toBe("none");
    expect(trayDot(snapshot([]), false, T)).toBe("none");
    expect(trayDot(snapshot([green("a", { intent: off, light: "gray", light_reason: "monitoring_off" })]), false, T)).toBe("none");
    expect(trayDot(snapshot([green("a")], { monitoring_paused: true }), false, T)).toBe("none");
    expect(trayDot(snapshot([green("a")]), true, T)).toBe("none");
    expect(trayDot(null, false, T)).toBe("none");
    const link: Link = { snapshot: snapshot([green("a")]), receivedAtMono: 0, lastError: null };
    expect(viewModel(link, LOST_AFTER_MS + 1).tray.dot).toBe("none");
    expect(viewModel(link, 1).tray.dot).toBe("green");
  });
});

describe("card switches and progress (wave three)", () => {
  it("shows the per-tunnel monitoring and notification switches as reported", () => {
    const paused = tunnel({
      light: "gray",
      light_reason: "monitoring_off",
      notifications_enabled: false,
      intent: { desired: "connected", monitoring_enabled: false, auto_recovery_enabled: true, start_when_stopped_externally: false },
    });
    const row = viewModel(fresh(snapshot([paused])), 1_000).rows[0]!;
    expect(row.monitoring).toEqual({ on: false, toggle: { kind: "enabled" }, note: "" });
    expect(row.notifications.on).toBe(false);
    expect(row.reason).toBe(LIGHT_REASON.monitoring_off);
    // Nothing is probed or read while it is paused.
    expect(row.probeNow.kind).toBe("disabled");
    expect(row.osState).toContain("暫停前");
    expect(row.failures).toBeNull();
  });

  it("says the tunnel's notification switch does nothing while Telegram is off", () => {
    const s = snapshot([tunnel()]);
    expect(viewModel(fresh(s), 1_000).rows[0]!.notifications.note).toContain("「通知」頁關閉");
    const on = snapshot([tunnel()], { notification: { ...s.notification, enabled: true } });
    expect(viewModel(fresh(on), 1_000).rows[0]!.notifications.note).toBe("");
  });

  it("explains why auto recovery cannot be turned on, under the switch too", () => {
    const blocked = tunnel({
      recovery_target: null,
      auto_recovery_allowed: false,
      intent: { desired: "connected", monitoring_enabled: true, auto_recovery_enabled: false, start_when_stopped_externally: false },
    });
    const row = viewModel(fresh(snapshot([blocked])), 1_000).rows[0]!;
    expect(row.autoRecovery.toggle).toEqual({ kind: "disabled", why: "請先設定復原判定目標" });
    expect(row.autoRecovery.note).toBe("請先設定復原判定目標");
    // While lost the overlay says why; no note on every card.
    const lost = viewModel({ snapshot: snapshot([blocked]), receivedAtMono: 0, lastError: null }, LOST_AFTER_MS + 1).rows[0]!;
    expect(lost.autoRecovery.note).toBe("");
  });

  it("shows the retry progress against retries + 1 while failing", () => {
    const failing = (n: number, retries = 2) =>
      viewModel(
        fresh(
          snapshot([
            tunnel({ light: "yellow", light_reason: "confirming", consecutive_failures: n, probe: { ...DEFAULT_PROBE_TIMING, retries } }),
          ]),
        ),
        1_000,
      ).rows[0]!.failures;
    expect(failing(0)).toBeNull();
    expect(failing(1)).toBe("連續失敗 1 / 3");
    expect(failing(3)).toBe("連續失敗 3 / 3");
    expect(failing(9)).toBe("連續失敗 9 次（判定需 3 次）");
    expect(failing(1, 0)).toBe("連續失敗 1 / 1");
  });

  it("shows the tunnel's own timing and the judgment time it gives", () => {
    const slow = tunnel({ probe: { interval_secs: 30, timeout_secs: 3, retries: 1, min_fault_secs: 90 }, freshness_secs: 51 });
    const row = viewModel(fresh(snapshot([slow])), 1_000).rows[0]!;
    expect(row.settings.probe).toEqual(slow.probe);
    expect(row.freshnessSecs).toBe(51);
    // k = max(1, ceil(90 / 30)) = 3: 93 to 123 s.
    expect(row.judgment).toBe("斷線後約 93～123 秒（約 1 分 33 秒～2 分 3 秒）判定故障（連續 2 次失敗，且至少 90 秒）");
    expect(row.judgmentShort).toBe("每 30 秒探測，約 1 分 33 秒～2 分 3 秒判定故障");
  });

  it("fills the settings form with the targets as they are typed", () => {
    const t = tunnel({
      recovery_target: { target: { address: "192.0.2.7", probe: { kind: "tcp_connect", port: 443 } }, last_probe: null },
      observe_targets: [
        { target: { address: "198.51.100.1", probe: { kind: "icmp" } }, last_probe: null },
        { target: { address: "198.51.100.2", probe: { kind: "tcp_connect", port: 445 } }, last_probe: null },
      ],
    });
    const s = viewModel(fresh(snapshot([t])), 1_000).rows[0]!.settings;
    expect(s).toEqual({
      recoveryAddress: "192.0.2.7",
      recoveryPort: "443",
      observeText: "198.51.100.1\n198.51.100.2:445",
      probe: DEFAULT_PROBE_TIMING,
      retry: DEFAULT_RECOVERY_RETRY,
      retrySupported: false,
    });
  });

  it("offers the recovery retry only from a watchdog that has it (1.2.0)", () => {
    const of = (version: string) => {
      const s = snapshot([tunnel()]);
      return viewModel(fresh({ ...s, host: { ...s.host, watchdog_version: version } }), 1_000).rows[0]!.settings;
    };
    expect(of("1.1.0").retrySupported).toBe(false);
    expect(of("1.2.0").retrySupported).toBe(true);
    expect(of("2.0.0").retrySupported).toBe(true);
    expect(of("1.2.0-rc1").retrySupported).toBe(false);
    // Nothing new is offered while the watchdog is lost.
    const s = snapshot([tunnel()]);
    const link: Link = { snapshot: { ...s, host: { ...s.host, watchdog_version: "1.2.0" } }, receivedAtMono: 0, lastError: null };
    expect(viewModel(link, LOST_AFTER_MS + 1).rows[0]!.settings.retrySupported).toBe(false);
  });

  it("says how far the incident's retries have got", () => {
    const incident = {
      incident_id: "inc-000007",
      opened_at_unix_ms: T - 600_000,
      first_recovery: "failed" as const,
      backoff_step: 3,
      stable_since_unix_ms: null,
      notification: "sent" as const,
      retries_used: 3,
      retries_exhausted: false,
      exhausted_notification: "not_eligible" as const,
    };
    const detail = (over: Partial<TunnelStatus>, version = "1.2.0") => {
      const s = snapshot([tunnel({ incident, ...over })]);
      const row = viewModel(fresh({ ...s, host: { ...s.host, watchdog_version: version } }), 1_000).rows[0]!;
      return row.details.find(([k]) => k === "自動復原重試")?.[1] ?? null;
    };
    expect(detail({})).toBe("已重試 3 次（上限 100 次）");
    expect(detail({ retry: { limit: null, interval_secs: 60 } })).toBe("已重試 3 次（持續重試）");
    expect(detail({ incident: { ...incident, retries_exhausted: true }, retry: { limit: 3, interval_secs: 60 } })).toBe(
      "已重試 3 次，已達上限 3 次，停止重試",
    );
    // An older watchdog's retry is only the contract's default: not shown.
    expect(detail({}, "1.1.0")).toBeNull();
    expect(LIGHT_REASON.retries_exhausted).toBe("自動復原已達重試上限，停止重試");
  });

  it("reads the recovery retry as typed, with its ranges", () => {
    expect(parseRetry({ forever: false, limit: "100", interval: "60" })).toEqual({
      ok: true,
      value: { limit: 100, interval_secs: 60 },
    });
    expect(parseRetry({ forever: true, limit: "x", interval: "10800" })).toEqual({
      ok: true,
      value: { limit: null, interval_secs: 10800 },
    });
    expect(parseRetry({ forever: false, limit: "0", interval: "10" })).toEqual({ ok: true, value: { limit: 0, interval_secs: 10 } });
    const error = (input: { forever: boolean; limit: string; interval: string }) => {
      const parsed = parseRetry(input);
      return parsed.ok ? null : parsed.error;
    };
    expect(error({ forever: false, limit: "3601", interval: "60" })).toBe("重試次數上限必須介於 0 到 3600");
    expect(error({ forever: false, limit: "-1", interval: "60" })).toBe("重試次數上限請輸入 0 到 3600 的整數");
    expect(error({ forever: false, limit: "5", interval: "9" })).toBe("重試間隔必須介於 10 到 10800");
    expect(error({ forever: true, limit: "", interval: "10801" })).toBe("重試間隔必須介於 10 到 10800");
  });

  it("offers removal while the watchdog answers", () => {
    expect(viewModel(fresh(snapshot([tunnel()])), 1_000).rows[0]!.unregister.kind).toBe("enabled");
    const link: Link = { snapshot: snapshot([tunnel()]), receivedAtMono: 0, lastError: null };
    expect(viewModel(link, LOST_AFTER_MS + 1).rows[0]!.unregister.kind).toBe("disabled");
  });

  it("shows a fault in words on the card, its own text only in the details", () => {
    const faulty = tunnel({
      light: "red",
      light_reason: "control_failed",
      fault: { kind: "permission_denied", message: "Access is denied. (os error 5)" },
    });
    const row = viewModel(fresh(snapshot([faulty])), 1_000).rows[0]!;
    expect(row.fault).toEqual({
      text: "權限不足，系統拒絕了這個操作",
      detail: "permission_denied: Access is denied. (os error 5)",
    });
    expect(row.reason).toBe(LIGHT_REASON.control_failed);
    // The card's details name the kind, never the message as the sentence.
    expect(row.details).toContainEqual(["錯誤", "權限不足（permission_denied）"]);
    expect(JSON.stringify(row.details)).not.toContain("Access is denied");
    // Lost: nothing about now is shown, the fault included.
    expect(viewModel({ ...fresh(snapshot([faulty])), receivedAtMono: 0 }, LOST_AFTER_MS + 1).rows[0]!.fault).toBeNull();
    expect(viewModel(fresh(snapshot([tunnel()])), 1_000).rows[0]!.fault).toBeNull();
  });

  it("shows a tunnel disconnected on purpose as intended", () => {
    const t = tunnel({
      light: "gray",
      light_reason: "disconnected_by_user",
      os_state: "disconnected",
      intent: { desired: "disconnected", monitoring_enabled: true, auto_recovery_enabled: true, start_when_stopped_externally: false },
    });
    const row = viewModel(fresh(snapshot([t])), 1_000).rows[0]!;
    expect(row.intended).toBe(true);
    expect(row.reason).toContain("不是故障");
    expect(row.disconnect.kind).toBe("disabled");
    expect(row.connect.kind).toBe("enabled");
  });
});

describe("times in the configured zone", () => {
  it("writes the test notification's time in the watchdog's zone", () => {
    const s = snapshot([], {
      notification: {
        enabled: true,
        device_name: "x",
        chat_id: null,
        token: "stored",
        last_test: { request_id: "gui-1", state: "accepted", requested_at_unix_ms: T, finished_at_unix_ms: T, failure: null },
        last_failure: null,
        templates_id: null,
        cli_language: null,
        host_name: null,
      },
    });
    // T is 2026-01-01 00:00:00 UTC.
    expect(notificationView(s).test).toBe("Telegram 已接受（2026-01-01 08:00:00）");
    const ny: Snapshot = {
      ...s,
      host: { ...s.host, time_zone: { configured: "America/New_York", effective: "America/New_York", utc_offset_secs: -18_000 } },
    };
    expect(notificationView(ny).test).toBe("Telegram 已接受（2025-12-31 19:00:00）");
  });

  it("names the watchdog's state and last update in the header", () => {
    expect(viewModel(fresh(snapshot([tunnel()])), 1_000).link).toEqual({ monitor: "ok", text: "● 背景監控正常（08:00:00 更新）" });
    const lost = viewModel({ snapshot: snapshot([tunnel()]), receivedAtMono: 0, lastError: null }, LOST_AFTER_MS + 1);
    expect(lost.link.text).toBe("■ 背景監控失聯（最後更新 2026-01-01 08:00:00）");
    expect(viewModel({ snapshot: null, receivedAtMono: null, lastError: null }, 0).link.text).toBe("■ 背景監控失聯");
  });
});

describe("a green light past its evidence (Codex P1-2)", () => {
  // The reviewer's case: the window asks every 300 s, so it shows the
  // watchdog as lost only after 900 s; the tunnel's results count for 35 s;
  // its last success is 1 s older than the snapshot, which arrived 100 s ago.
  const slow = (s: Snapshot = snapshot([tunnel()])): Link => ({
    snapshot: s,
    receivedAtMono: 0,
    lastError: null,
    lostAfterMs: lostAfterMs(300),
  });

  it("shows no green and no green dot once the evidence is older than its freshness", () => {
    const vm = viewModel(slow(), 100_000);
    // The link itself is fine: no lost-link overlay, the owner's rule.
    expect(vm.overlay).toBeNull();
    expect(vm.tray.monitor).toBe("ok");
    const row = vm.rows[0]!;
    expect(row.light).not.toBe("green");
    expect(row.icon).not.toBe(LIGHT.green.icon);
    expect(row.lightLabel).not.toBe(LIGHT.green.label);
    expect(row.expired).toBe("結果已過期，等待下一次更新");
    expect(row.reason).not.toBe(LIGHT_REASON.verified);
    expect(row.reason).toContain("35 秒");
    expect(vm.tray.dot).toBe("none");
    expect(trayDot(slow().snapshot, false, T + 100_000)).toBe("none");
    // The header counts it apart, never as green.
    expect(vm.summary).not.toContain(LIGHT.green.label);
    expect(vm.summary).toContain("結果已過期 1");
    expect(expiredTunnels(slow(), 100_000)).toEqual(["office-1"]);
  });

  it("keeps green exactly while the last success is within its freshness", () => {
    // Last success at T − 1 s, 35 s fresh: current until T + 34 s.
    expect(viewModel(slow(), 34_000).rows[0]!.light).toBe("green");
    expect(viewModel(slow(), 34_000).rows[0]!.expired).toBeNull();
    expect(viewModel(slow(), 34_000).tray.dot).toBe("green");
    expect(viewModel(slow(), 34_001).rows[0]!.light).toBe("unknown");
    expect(viewModel(slow(), 34_001).tray.dot).toBe("none");
    expect(expiredTunnels(slow(), 34_000)).toEqual([]);
    // A longer freshness (a 600 s interval, 10 s timeout) lasts longer.
    const long = snapshot([tunnel({ freshness_secs: 628 })]);
    expect(viewModel(slow(long), 627_000).rows[0]!.light).toBe("green");
    expect(viewModel(slow(long), 627_001).rows[0]!.light).toBe("unknown");
    // The snapshot's own freshness counts, never one worked out here: the
    // default timing with two observe targets is 35 + 2 × 3 = 41 s.
    const observed = snapshot([tunnel({ freshness_secs: 41 })]);
    expect(viewModel(slow(observed), 40_000).rows[0]!.light).toBe("green");
    expect(viewModel(slow(observed), 40_001).rows[0]!.light).toBe("unknown");
  });

  it("dates the evidence by the recovery target's last answer when there is no last success, and by nothing else", () => {
    const answered = tunnel({ last_verified_unix_ms: null });
    // Its last probe finished at T − 1 s too.
    expect(viewModel(slow(snapshot([answered])), 34_000).rows[0]!.light).toBe("green");
    expect(viewModel(slow(snapshot([answered])), 34_001).rows[0]!.light).toBe("unknown");
    // A failed last probe is no evidence of green, and neither is none: such
    // a green is shown as expired at once.
    const failed = tunnel({
      last_verified_unix_ms: null,
      recovery_target: {
        target: { address: "192.0.2.1", probe: { kind: "icmp" } },
        last_probe: { started_at_unix_ms: T - 1_020, finished_at_unix_ms: T - 1_000, result: { result: "no_reply" } },
      },
    });
    expect(viewModel(slow(snapshot([failed])), 0).rows[0]!.expired).not.toBeNull();
    const none = tunnel({ last_verified_unix_ms: null, recovery_target: null });
    expect(viewModel(slow(snapshot([none])), 0).rows[0]!.expired).not.toBeNull();
  });

  it("fixes the snapshot's age when it arrives, by the round trip or the wall clock, whichever is longer", () => {
    // Sent at 0, arrived at 10 s: no younger than the round trip, whatever
    // the wall clock says, set back meanwhile included.
    expect(arrivalAgeMs(0, 10_000, T, T + 10_000)).toBe(10_000);
    expect(arrivalAgeMs(0, 10_000, T, T - 50_000)).toBe(10_000);
    // The wall clock says it is older still: that counts.
    expect(arrivalAgeMs(9_990, 10_000, T, T + 40_000)).toBe(40_000);
    expect(arrivalAgeMs(10_000, 10_000, T, T - 1)).toBe(0);
    const link = slow();
    expect(watchdogNowMs(link, 10_000)).toBe(T + 10_000);
    const late: Link = { ...slow(), ageOnArrivalMs: 40_000 };
    expect(watchdogNowMs(late, 10_000)).toBe(T + 50_000);
    expect(viewModel(late, 0).rows[0]!.expired).not.toBeNull();
    // Only the monotonic clock ages it afterwards.
    expect(watchdogNowMs(link, 100_000)).toBe(T + 100_000);
    expect(viewModel(link, 100_000).rows[0]!.light).toBe("unknown");
    expect(watchdogNowMs({ snapshot: null, receivedAtMono: null, lastError: null }, 0)).toBeNull();
  });

  it("leaves every other light as the watchdog gave it", () => {
    const others = [
      tunnel({ tunnel_id: "y", light: "yellow", light_reason: "confirming", deadline_unix_ms: T + 30_000 }),
      tunnel({ tunnel_id: "r", light: "red", light_reason: "fault_confirmed" }),
      tunnel({ tunnel_id: "g", light: "gray", light_reason: "not_connected", os_state: "disconnected" }),
      // After a wake or a network change the old success is still reported
      // while the light is the grace period's yellow.
      tunnel({ tunnel_id: "w", light: "yellow", light_reason: "grace_period", deadline_unix_ms: T + 30_000, last_verified_unix_ms: T - 600_000 }),
    ];
    const vm = viewModel(slow(snapshot(others)), 100_000);
    expect(vm.rows.map((r) => r.light)).toEqual(["yellow", "red", "gray", "yellow"]);
    expect(vm.rows.every((r) => r.expired === null)).toBe(true);
    // A red light still gives the red dot, an expired green beside it or not.
    expect(viewModel(slow(snapshot([tunnel(), others[1]!])), 100_000).tray.dot).toBe("red");
    // Lost is lost: the lost-link overlay, not the expired one.
    const lost = viewModel(slow(), lostAfterMs(300) + 1).rows[0]!;
    expect(lost.light).toBe("unknown");
    expect(lost.expired).toBeNull();
  });

  it("asks for one snapshot 2 s before a green expires, so a healthy watchdog's green has no gap", () => {
    // The reviewer's numbers: last success at T − 1 s, 35 s fresh, so the
    // evidence expires at T + 34 s; the snapshot arrived at 0.
    const link = slow();
    const watch = new ExpiryWatch();
    watch.arrived(link.snapshot!, 0, []);
    expect(watch.shouldRefresh(link, 31_999)).toBe(false);
    expect(watch.shouldRefresh(link, 32_000)).toBe(true);
    // Once per snapshot, however often it is asked.
    expect(watch.shouldRefresh(link, 32_001)).toBe(false);
    expect(watch.shouldRefresh(link, 100_000)).toBe(false);
    // Asked for, it is still green: the notice waits for the real expiry.
    expect(viewModel(link, 32_000).rows[0]!.light).toBe("green");
    expect(viewModel(link, 32_000).tray.dot).toBe("green");
    expect(viewModel(link, 34_001).rows[0]!.expired).not.toBeNull();
    // Its age on arrival counts here too.
    const walled = new ExpiryWatch();
    const late: Link = { ...link, ageOnArrivalMs: 22_000 };
    walled.arrived(late.snapshot!, 0, []);
    expect(walled.shouldRefresh(late, 9_999)).toBe(false);
    expect(walled.shouldRefresh(late, 10_000)).toBe(true);
  });

  it("never asks sooner than 1 s after a snapshot arrived, nor in a loop", () => {
    // Expires 1.5 s after the snapshot: due at once, asked only at 1 s.
    const soon = slow(snapshot([tunnel({ last_verified_unix_ms: T - 33_500 })]));
    const watch = new ExpiryWatch();
    watch.arrived(soon.snapshot!, 0, []);
    expect(watch.shouldRefresh(soon, 999)).toBe(false);
    expect(watch.shouldRefresh(soon, 1_000)).toBe(true);
    // A green already expired when its snapshot arrived is the newest news
    // there is: it does not set off another request.
    const late = slow(snapshot([tunnel({ last_verified_unix_ms: T - 40_000 })]));
    watch.arrived(late.snapshot!, 0, expiredTunnels(late, 0));
    expect(expiredTunnels(late, 0)).toEqual(["office-1"]);
    expect(watch.shouldRefresh(late, 1_000)).toBe(false);
    expect(watch.shouldRefresh(late, 100_000)).toBe(false);
    // Another green of it that expires later does, once.
    const mixed = slow(
      snapshot([tunnel({ last_verified_unix_ms: T - 40_000 }), tunnel({ tunnel_id: "lab-2" })]),
    );
    watch.arrived(mixed.snapshot!, 0, expiredTunnels(mixed, 0));
    expect(watch.shouldRefresh(mixed, 31_999)).toBe(false);
    expect(watch.shouldRefresh(mixed, 32_000)).toBe(true);
    expect(watch.shouldRefresh(mixed, 32_000)).toBe(false);
    // Nothing for other lights, for a snapshot it was not told about, or
    // while the watchdog is lost.
    const yellow = slow(snapshot([tunnel({ light: "yellow", light_reason: "grace_period", deadline_unix_ms: T + 30_000 })]));
    watch.arrived(yellow.snapshot!, 0, []);
    expect(watch.shouldRefresh(yellow, 100_000)).toBe(false);
    expect(watch.shouldRefresh(slow(), 100_000)).toBe(false);
    const lost = slow();
    watch.arrived(lost.snapshot!, 0, []);
    expect(watch.shouldRefresh(lost, lostAfterMs(300) + 1)).toBe(false);
  });
});

describe("the tray's green dot on the shell's own clock (Codex re-review P1-2)", () => {
  // The same case: the window asks every 300 s; the tunnel's results count
  // for 35 s; its last success is 1 s older than the snapshot, which arrived
  // at 0, so the evidence runs out at T + 34 s.
  const slow = (s: Snapshot = snapshot([tunnel()])): Link => ({
    snapshot: s,
    receivedAtMono: 0,
    lastError: null,
    lostAfterMs: lostAfterMs(300),
  });
  const monitoringOff = {
    desired: "connected",
    monitoring_enabled: false,
    auto_recovery_enabled: false,
    start_when_stopped_externally: false,
  } as const;

  it("tells the shell how long the green dot may stay from now", () => {
    expect(viewModel(slow(), 0).tray.greenForMs).toBe(34_000);
    expect(viewModel(slow(), 10_000).tray.greenForMs).toBe(24_000);
    expect(viewModel(slow(), 34_000).tray).toMatchObject({ dot: "green", greenForMs: 0 });
    expect(viewModel(slow(), 34_001).tray).toMatchObject({ dot: "none", greenForMs: 0 });
    // Its age on arrival counts here too.
    expect(viewModel({ ...slow(), ageOnArrivalMs: 20_000 }, 10_000).tray.greenForMs).toBe(4_000);
  });

  it("never lengthens the deadline for the same snapshot when the wall clock is set back (Codex P1-N3)", () => {
    // The reviewer's sequence: made at T, last success T − 1 s, 35 s fresh,
    // so the evidence runs out at T + 34 s. Asked for at 0, it arrives at
    // 10 s; at 15 s the wall clock goes back 60 s and the same snapshot is
    // drawn again. The deadline the shell is given, counted from when it is
    // given, must stay at 34 s, never 44 s.
    const link: Link = { ...slow(), receivedAtMono: 10_000, ageOnArrivalMs: arrivalAgeMs(0, 10_000, T, T + 10_000) };
    const deadlines = [10_000, 15_000, 20_000, 33_000].map((now) => now + viewModel(link, now).tray.greenForMs);
    expect(deadlines).toEqual([34_000, 34_000, 34_000, 34_000]);
    expect(viewModel(link, 34_001).tray).toMatchObject({ dot: "none", greenForMs: 0 });
    // The clock set back while the answer was on its way changes nothing
    // either: the round trip still counts.
    const setBack: Link = { ...link, ageOnArrivalMs: arrivalAgeMs(0, 10_000, T, T - 50_000) };
    expect(10_000 + viewModel(setBack, 10_000).tray.greenForMs).toBe(34_000);
  });

  it("gives the first of the greens that make the dot green, and nothing without a green dot", () => {
    const second = tunnel({ tunnel_id: "lab-2", last_verified_unix_ms: T - 20_000 });
    expect(viewModel(slow(snapshot([tunnel(), second])), 0).tray.greenForMs).toBe(15_000);
    // A tunnel that is not monitored does not make the dot green.
    const unmonitored = tunnel({ tunnel_id: "lab-3", last_verified_unix_ms: T - 30_000, intent: monitoringOff });
    expect(viewModel(slow(snapshot([tunnel(), unmonitored])), 0).tray.greenForMs).toBe(34_000);
    const red = tunnel({ tunnel_id: "r", light: "red", light_reason: "fault_confirmed" });
    const yellow = tunnel({ tunnel_id: "y", light: "yellow", light_reason: "confirming", deadline_unix_ms: T + 30_000 });
    for (const s of [
      snapshot([tunnel(), red]),
      snapshot([tunnel(), yellow]),
      snapshot([]),
      snapshot([tunnel()], { monitoring_paused: true }),
    ]) {
      const summary = viewModel(slow(s), 0).tray;
      expect(summary.dot).not.toBe("green");
      expect(summary.greenForMs).toBe(0);
    }
    expect(viewModel(slow(), lostAfterMs(300) + 1).tray).toMatchObject({ dot: "none", greenForMs: 0 });
  });

  it("never promises more than the shell takes", () => {
    expect(TRAY_GREEN_MAX_MS).toBe(3_700_000);
    // The longest freshness there is: 3,600 + 10 + 18 + 4 × 11 = 3,672 s.
    const longest = snapshot([tunnel({ freshness_secs: 3_672, last_verified_unix_ms: T })]);
    expect(viewModel(slow(longest), 0).tray.greenForMs).toBe(3_672_000);
    // A success stamped after the snapshot (the watchdog's clock moved back).
    const ahead = snapshot([tunnel({ last_verified_unix_ms: T + 10_000_000 })]);
    expect(viewModel(slow(ahead), 0).tray.greenForMs).toBe(TRAY_GREEN_MAX_MS);
    // Whole milliseconds only.
    const odd = snapshot([tunnel({ last_verified_unix_ms: T - 1_000 })]);
    expect(Number.isInteger(viewModel(slow(odd), 0.5).tray.greenForMs)).toBe(true);
  });

  it("does not call an expired green normal", () => {
    expect(viewModel(slow(), 34_000).tray.level).toBe("ok");
    expect(viewModel(slow(), 34_001).tray.level).toBe("expired");
    // A VPN fault still comes first, and a lost link above all.
    const red = tunnel({ tunnel_id: "r", light: "red", light_reason: "fault_confirmed" });
    expect(viewModel(slow(snapshot([tunnel(), red])), 100_000).tray.level).toBe("fault");
    expect(viewModel(slow(), lostAfterMs(300) + 1).tray.level).toBe("lost");
    // A host target down beside it does not hide that the green has expired.
    const host = tunnel({
      observe_targets: [
        {
          target: { address: "192.0.2.10", probe: { kind: "tcp_connect", port: 445 } },
          last_probe: { started_at_unix_ms: T, finished_at_unix_ms: T, result: { result: "no_reply" } },
        },
      ],
    });
    expect(viewModel(slow(snapshot([host])), 0).tray.level).toBe("host");
    expect(viewModel(slow(snapshot([host])), 100_000).tray).toMatchObject({ level: "expired", hostIssues: 1 });
  });

  it("sends the shell numbers and fixed choices only, the deadline among them", () => {
    const summary = viewModel(slow(), 10_000).tray;
    expect(trayCommand(summary, 300)).toEqual({
      level: "ok",
      vpn_faults: 0,
      host_issues: 0,
      monitor: "ok",
      dot: "green",
      refresh_secs: 300,
      green_for_ms: 24_000,
    });
    // Counts are clamped to what the shell takes.
    expect(trayCommand({ ...summary, vpnFaults: 300, hostIssues: 999 }, 5)).toMatchObject({
      vpn_faults: 255,
      host_issues: 255,
      refresh_secs: 5,
    });
    expect(trayCommand(viewModel(slow(), 34_001).tray, 300)).toMatchObject({
      level: "expired",
      dot: "none",
      green_for_ms: 0,
    });
  });
});
