// The log export, without the DOM: anonymous names, what the text says,
// what it never says, every kind of log line, and reading the log page by
// page up to the cap.

import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import { MAX_EVENT_PAGE, MIN_USABLE_WATCHDOG_VERSION, MIN_WATCHDOG_VERSION, type EventEntry, type LogEvent, type Snapshot, type TunnelStatus } from "./protocol";
import {
  addressMask,
  EXPORT_CAP,
  ExportNames,
  type ReportInput,
  buildReport,
  collectLog,
  platformOf,
  rawFields,
} from "./report";
import { MASK } from "./scrub";

/// 2026-01-01 00:00:00 UTC, 08:00 in Taipei.
const T = 1_767_225_600_000;
const WINDOWS_UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Edg/140.0";

function tunnel(id: string, name: string, over: Partial<TunnelStatus> = {}): TunnelStatus {
  return {
    tunnel_id: id,
    display_name: name,
    light: "green",
    light_reason: "verified",
    light_since_unix_ms: T - 60_000,
    deadline_unix_ms: null,
    next_retry_unix_ms: null,
    last_verified_unix_ms: T - 1_000,
    intent: { desired: "connected", monitoring_enabled: true, auto_recovery_enabled: true, start_when_stopped_externally: false },
    auto_recovery_allowed: true,
    notifications_enabled: true,
    takeover: "managed",
    os_state: "connected",
    data_health: "healthy",
    recovery: "idle",
    recovery_target: {
      target: { address: "192.0.2.1", probe: { kind: "icmp" } },
      last_probe: { started_at_unix_ms: T - 1_020, finished_at_unix_ms: T - 1_000, result: { result: "reply", rtt_ms: 12 } },
    },
    observe_targets: [
      {
        target: { address: "198.51.100.7", probe: { kind: "tcp_connect", port: 445 } },
        last_probe: { started_at_unix_ms: T - 2_000, finished_at_unix_ms: T - 1_900, result: { result: "no_reply" } },
      },
    ],
    probe: { interval_secs: 15, timeout_secs: 2, retries: 2, min_fault_secs: 60 },
    freshness_secs: 35,
    consecutive_failures: 0,
    fault: null,
    incident: null,
    retry: { limit: 100, interval_secs: 60 },
    recovered_notification: null,
    auto_recovery_pending: false,
    ...over,
  };
}

const OFFICE = tunnel("office-uuid-1", "辦公室");
const LAB = tunnel("lab-2", "Lab", {
  light: "red",
  light_reason: "recovery_failed",
  next_retry_unix_ms: T + 60_000,
  recovery: "backoff",
  consecutive_failures: 9,
  fault: { kind: "timeout", message: "probe of 192.0.2.2 via C:\\Users\\alice\\x timed out" },
  incident: {
    incident_id: "inc-0007",
    opened_at_unix_ms: T - 600_000,
    first_recovery: "failed",
    backoff_step: 2,
    stable_since_unix_ms: null,
    notification: "sent",
    retries_used: 2,
    retries_exhausted: false,
    exhausted_notification: "not_eligible",
  },
});

function snapshot(tunnels: TunnelStatus[] = [OFFICE, LAB]): Snapshot {
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
      background_operation: "supported",
    },
    monitoring_paused: false,
    host: {
      watchdog_version: "4.2.1",
      min_gui_version: "1.0.0",
      min_usable_gui_version: "1.0.0",
      mcp_enabled: false,
      mcp_telegram_enabled: false,
      workers: { configured: null, suggested: 3, limit: 3, effective: 3, running: 2 },
      time_zone: { configured: null, effective: "Asia/Taipei", utc_offset_secs: 28_800 },
    },
    notification: {
      enabled: true,
      device_name: "Alice 的筆電",
      chat_id: "-1001234567890",
      token: "stored",
      last_test: null,
      last_failure: null,
      templates_id: null,
      cli_language: null,
      host_name: null,
    },
    tunnels,
  };
}

/// One line of every kind. A missing kind is a type error.
const SAMPLES: { [K in LogEvent["kind"]]: Extract<LogEvent, { kind: K }> } = {
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

/// Every kind once, newest first, about the lab tunnel.
const ENTRIES: EventEntry[] = Object.values(SAMPLES)
  .map((event, i): EventEntry => ({ seq: 1000 + i, at_unix_ms: T - (100 - i) * 1_000, tunnel_id: "lab-2", removed_tunnel: false, event }))
  .reverse();

function input(over: Partial<ReportInput> = {}): ReportInput {
  return {
    snapshot: snapshot(),
    lost: false,
    lastError: null,
    entries: ENTRIES,
    truncated: false,
    logError: null,
    includeNames: false,
    nowMs: T + 5_000,
    guiVersion: "2.1.0",
    userAgent: WINDOWS_UA,
    linkRecords: [],
    linkStored: true,
    ...over,
  };
}

describe("names in an export", () => {
  it("numbers tunnels in snapshot order unless names are asked for", () => {
    const anonymous = new ExportNames([OFFICE, LAB], false);
    expect(anonymous.tunnel("office-uuid-1")).toBe("隧道 1");
    expect(anonymous.tunnel("lab-2")).toBe("隧道 2");
    const named = new ExportNames([OFFICE, LAB], true);
    expect(named.tunnel("office-uuid-1")).toBe("辦公室");
    expect(named.withNames).toBe(true);
    // Two tunnels with one name keep their IDs apart, as on the cards.
    const twins = new ExportNames([tunnel("a", "Office"), tunnel("b", "Office")], true);
    expect([twins.tunnel("a"), twins.tunnel("b")]).toEqual(["Office（a）", "Office（b）"]);
  });

  it("masks what a name or a tunnel ID carries, even with names asked for (Codex P2-6)", () => {
    const uuid = "6f9a2c1e-1b2c-4d5e-8f90-0123456789ab";
    const named = new ExportNames(
      [
        tunnel("t1", "office-192.0.2.1"),
        tunnel("t2", `vpn ${uuid}`),
        tunnel("t3", "peer vpn.example.com:51820"),
        tunnel("t4", "[2001:db8::1]:51820 link"),
        tunnel("t5", "C:\\Users\\Bob\\vpn"),
        tunnel("t6", "chat -1001234567890"),
      ],
      true,
    );
    expect(["t1", "t2", "t3", "t4", "t5", "t6"].map((id) => named.tunnel(id))).toEqual([
      `office-${MASK.ipv4}`,
      `vpn ${MASK.uuid}`,
      `peer ${MASK.endpoint}`,
      `${MASK.endpoint} link`,
      `C:\\Users\\${MASK.account}\\vpn`,
      `chat ${MASK.chat}`,
    ]);
    // The ID added to tell twins apart is masked too; where masking makes
    // two labels alike, their numbers keep them apart.
    const twins = new ExportNames(
      [tunnel("office-1", "Office"), tunnel(uuid, "Office"), tunnel("6f9a2c1e-1b2c-4d5e-8f90-0123456789ac", "Office")],
      true,
    );
    expect(twins.tunnel("office-1")).toBe("Office（office-1）");
    expect(twins.tunnel(uuid)).toBe(`Office（${MASK.uuid}）（隧道 2）`);
    expect(twins.tunnel("6f9a2c1e-1b2c-4d5e-8f90-0123456789ac")).toBe(`Office（${MASK.uuid}）（隧道 3）`);
    const alike = new ExportNames([tunnel("a", "site 192.0.2.1"), tunnel("b", "site 192.0.2.2"), tunnel("c", "home")], true);
    expect(["a", "b", "c"].map((id) => alike.tunnel(id))).toEqual([
      `site ${MASK.ipv4}（隧道 1）`,
      `site ${MASK.ipv4}（隧道 2）`,
      "home",
    ]);
    // In the whole text: nothing of the names' addresses or IDs is left.
    const text = buildReport(
      input({
        includeNames: true,
        snapshot: snapshot([tunnel("t1", "office-192.0.2.1"), tunnel(uuid, "dup"), tunnel("t3", "dup")]),
        entries: [{ seq: 1, at_unix_ms: T, tunnel_id: uuid, removed_tunnel: false, event: { kind: "user_connect" } }],
      }),
    );
    expect(text).toContain(`[office-${MASK.ipv4}]`);
    expect(text).toContain(`[dup（${MASK.uuid}）]`);
    expect(text).toContain(` | dup（${MASK.uuid}） | `);
    for (const secret of ["192.0.2.", uuid]) expect(text, secret).not.toContain(secret);
  });

  it("labels host lines, removed tunnels and tunnels not in the snapshot", () => {
    const names = new ExportNames([OFFICE, LAB], false);
    const line = (tunnel_id: string | null, removed_tunnel = false): EventEntry => ({
      seq: 1,
      at_unix_ms: T,
      tunnel_id,
      removed_tunnel,
      event: { kind: "user_connect" },
    });
    expect(names.entry(line(null))).toBe("整體");
    expect(names.entry(line(null, true))).toBe("已移除的隧道");
    expect(names.entry(line("lab-2"))).toBe("隧道 2");
    // The same unknown tunnel gets the same next number every time.
    expect(names.entry(line("gone-9"))).toBe("隧道 3（不在目前清單）");
    expect(names.entry(line("gone-9"))).toBe("隧道 3（不在目前清單）");
    expect(names.entry(line("gone-10"))).toBe("隧道 4（不在目前清單）");
  });
});

describe("the export text", () => {
  const text = buildReport(input());

  it("starts with the header the brief lists", () => {
    const lines = text.split("\n");
    expect(lines.slice(0, 18)).toEqual([
      "WG Always Live 紀錄匯出",
      "匯出時間：2026-01-01 08:00:05 +08:00",
      "介面版本：2.1.0",
      "WatchDog 版本：4.2.1",
      // Both of this window's minimums are 4.2.1 since the one-version sync,
      // and equal minimums are written once.
      MIN_WATCHDOG_VERSION === MIN_USABLE_WATCHDOG_VERSION
        ? `介面的需求：需要 WatchDog ≥ ${MIN_WATCHDOG_VERSION}`
        : `介面的需求：完整功能需要 WatchDog ≥ ${MIN_WATCHDOG_VERSION}；隧道功能需要 ≥ ${MIN_USABLE_WATCHDOG_VERSION}`,
      "WatchDog 的需求：WatchDog 需要介面 ≥ 1.0.0",
      "相容性：✓ 相容",
      "通訊協定版本：1",
      "平台：Windows",
      "時區：設定 跟隨系統，使用 Asia/Taipei（UTC+08:00）",
      "背景監控：可連線（快照 2026-01-01 08:00:00 +08:00）",
      "儲存：正常（ok）",
      "全部監看：進行中",
      "工作執行緒：設定=自動 建議=3 上限=3 生效=3 執行中=2",
      "能力：query_state=supported start_stop=supported bound_icmp_probe=supported bound_tcp_probe=unknown read_metrics=unsupported verified_concurrent_tunnels=1 background_operation=supported（支援）",
      "Telegram 通知：開啟（不匯出 Bot Token 與 Chat ID）",
      "隧道名稱：已以「隧道 n」代替",
      "遮蔽：目標位址一律遮蔽；IP 位址、端點、UUID、帳號路徑、金鑰樣式字串與 Chat ID 一律遮蔽。",
    ]);
  });

  it("describes each tunnel with its state, timing and targets, addresses masked", () => {
    for (const line of [
      "== 隧道（依清單順序，共 2 條）==",
      "[隧道 1]",
      "燈號：green 正常；verified 經這條隧道的探測成功（自 2026-01-01 07:59:00 +08:00）",
      "意圖：desired=connected monitoring=on auto_recovery=on start_when_stopped_externally=off notifications=on auto_recovery_allowed=on",
      "探測：interval=15s timeout=2s retries=2 min_fault=60s；斷線後約 62～77 秒判定故障（連續 3 次失敗，且至少 60 秒）",
      "連續失敗：0 / 3",
      "新鮮度：35 秒",
      "最後成功：2026-01-01 07:59:59 +08:00",
      `復原判定目標：address=${addressMask()} probe=icmp last=reply rtt=12ms started=2026-01-01 07:59:58 +08:00 finished=2026-01-01 07:59:59 +08:00`,
      `僅監看目標 1：address=${addressMask()} probe=tcp_connect port=445 last=no_reply started=2026-01-01 07:59:58 +08:00 finished=2026-01-01 07:59:58 +08:00`,
      "[隧道 2]",
      "連續失敗：9 / 3",
      "期限：—；下次重試：2026-01-01 08:01:00 +08:00",
      "復原流程：backoff 等待重試中",
      `錯誤：timeout 逾時：probe of ${MASK.ipv4} via C:\\Users\\${MASK.account}\\x timed out`,
      "事件：id=inc-0007 opened=2026-01-01 07:50:00 +08:00 first_recovery=failed（失敗） backoff_step=2 stable_since=— notification=sent（已送出）",
    ]) {
      expect(text.split("\n"), line).toContain(line);
    }
  });

  it("never carries names, IDs, addresses, the device name or the chat ID", () => {
    for (const secret of ["辦公室", "Lab", "office-uuid-1", "lab-2", "192.0.2.", "198.51.100.", "alice", "Alice", "-1001234567890"]) {
      expect(text, secret).not.toContain(secret);
    }
  });

  it("keeps names, but still no addresses or chat ID, when names are asked for", () => {
    const named = buildReport(input({ includeNames: true }));
    expect(named).toContain("[辦公室]");
    expect(named).toContain("[Lab]");
    expect(named).toContain("隧道名稱：包含（只給自己分析用；傳給別人前請重新匯出並取消勾選）");
    for (const secret of ["192.0.2.", "198.51.100.", "alice", "-1001234567890", "Alice 的筆電"]) {
      expect(named, secret).not.toContain(secret);
    }
  });

  it("writes every kind of log line, newest first, with its raw fields", () => {
    expect(text).toContain(`== 事件紀錄（最新的在前，共 ${ENTRIES.length} 筆）==`);
    const logLines = text.split("\n").filter((l) => / \| seq=\d+ /.test(l));
    expect(logLines).toHaveLength(Object.keys(SAMPLES).length);
    for (const kind of Object.keys(SAMPLES)) {
      expect(logLines.some((l) => l.includes(` kind=${kind}`)), kind).toBe(true);
    }
    expect(logLines).toContain(
      "2026-01-01 07:58:32 +08:00 | 隧道 2 | 異常 | 探測失敗（連續 2 / 3） | seq=1012 kind=probe_failed streak=2 needed=3",
    );
    // Newest first.
    const seqs = logLines.map((l) => Number(/seq=(\d+)/.exec(l)?.[1]));
    expect(seqs).toEqual([...seqs].sort((a, b) => b - a));
  });

  it("marks lines about removed tunnels and host lines", () => {
    const entries: EventEntry[] = [
      { seq: 2, at_unix_ms: T, tunnel_id: null, removed_tunnel: true, event: { kind: "user_disconnect" } },
      { seq: 1, at_unix_ms: T, tunnel_id: null, removed_tunnel: false, event: { kind: "watchdog_started" } },
    ];
    const lines = buildReport(input({ entries })).split("\n");
    expect(lines).toContain("2026-01-01 08:00:00 +08:00 | 已移除的隧道 | 人工 | 使用者斷線 | seq=2 removed_tunnel=true kind=user_disconnect");
    expect(lines).toContain("2026-01-01 08:00:00 +08:00 | 整體 | 系統 | 背景監控已啟動 | seq=1 kind=watchdog_started");
  });

  it("says when the watchdog is lost, the log was cut or failed", () => {
    const lost = buildReport(input({ lost: true, lastError: "pipe 10.1.2.3 closed", truncated: true, logError: "timeout" }));
    expect(lost).toContain(`背景監控：失聯（pipe ${MASK.ipv4} closed）；以下為最後一次快照（2026-01-01 08:00:00 +08:00）`);
    expect(lost).toContain(`已達匯出上限 ${EXPORT_CAP} 筆，更早的紀錄沒有匯出。`);
    expect(lost).toContain("讀取紀錄中斷：timeout；以下只有已讀到的部分。");
    const none = buildReport(input({ snapshot: null, lost: true, entries: [] }));
    expect(none).toContain("背景監控：失聯，沒有任何快照");
    expect(none).toContain("== 隧道（依清單順序，共 0 條）==");
    expect(none).toContain("（沒有紀錄）");
    expect(none).not.toContain("工作執行緒");
    expect(none).toContain("WatchDog 版本：無法取得（沒有快照）");
    expect(lost).toContain("WatchDog 版本：4.2.1（最後一次快照）");
    expect(buildReport(input({ guiVersion: null }))).toContain("介面版本：無法取得");
  });

  it("carries both versions, both minimums and the verdict", () => {
    const withHost = (watchdog_version: string, min_gui_version: string) => {
      const s = snapshot();
      return { ...s, host: { ...s.host, watchdog_version, min_gui_version } };
    };
    // Different but new enough: compatible, no warning.
    const newer = buildReport(input({ snapshot: withHost("4.2.5", "1.0.0") }));
    expect(newer).toContain("WatchDog 版本：4.2.5");
    expect(newer).toContain("相容性：✓ 相容");
    const guiOld = buildReport(input({ snapshot: withHost("4.2.4", "1.2.0"), guiVersion: "1.1.9" }));
    expect(guiOld).toContain("WatchDog 的需求：完整功能需要介面 ≥ 1.2.0；隧道功能需要 ≥ 1.0.0");
    expect(guiOld).toContain("相容性：⚠ 介面版本過舊-無法支援完整功能，建議升級（WatchDog 需要 1.2.0 以上）");
    const lost = buildReport(input({ snapshot: withHost("4.2.4", "1.2.0"), guiVersion: "1.1.9", lost: true }));
    expect(lost).toContain("WatchDog 的需求：完整功能需要介面 ≥ 1.2.0；隧道功能需要 ≥ 1.0.0（最後一次快照）");
    expect(lost).toContain(
      "相容性：⚠ 介面版本過舊-無法支援完整功能，建議升級（WatchDog 需要 1.2.0 以上）（依最後一次快照）",
    );
    const none = buildReport(input({ snapshot: null, lost: true }));
    expect(none).toContain("WatchDog 的需求：無法取得（沒有快照）");
    expect(none).toContain("相容性：無法判斷（沒有快照）");
    expect(buildReport(input({ guiVersion: null }))).toContain("相容性：無法判斷（沒有介面版本）");
  });

  it("lists what the window saw of its link to the watchdog, in its own section", () => {
    const records = [
      { id: 2, atMs: T + 20_000, event: { kind: "restored", goneMs: 15_000 } },
      { id: 1, atMs: T + 5_000, event: { kind: "lost", lastOkMs: T, error: "not_running" } },
    ] as const;
    const lines = buildReport(input({ linkRecords: records })).split("\n");
    const at = lines.indexOf("== 介面觀察到的 WatchDog 連線狀況（最新的在前，共 2 筆）==");
    expect(at).toBeGreaterThan(lines.findIndex((l) => l.startsWith("== 事件紀錄")));
    expect(lines.slice(at + 1, at + 4)).toEqual([
      "由這個視窗記錄，存在這台電腦上，保留約 72 小時、最多 500 筆。",
      "2026-01-01 08:00:20 +08:00 | 介面觀察 | 系統 | 恢復連線（失聯約 15 秒） | id=2 kind=link_restored gone_ms=15000",
      "2026-01-01 08:00:05 +08:00 | 介面觀察 | 系統 | 無法連上 WatchDog：背景監控服務未執行或未安裝（最後一次正常：2026-01-01 08:00:00 +08:00） | id=1 kind=link_lost error=not_running last_ok_unix_ms=1767225600000",
    ]);
    const none = buildReport(input({ linkStored: false }));
    expect(none).toContain(
      [
        "== 介面觀察到的 WatchDog 連線狀況（最新的在前，共 0 筆）==",
        "由這個視窗記錄；這台電腦無法保存，只有這次開啟視窗以來的紀錄。",
        "（沒有紀錄）",
      ].join("\n"),
    );
  });

  it("scrubs strings in raw fields and knows the platform", () => {
    expect(rawFields({ kind: "fault_confirmed", incident_id: "inc-192.0.2.1" })).toBe(`kind=fault_confirmed incident_id=inc-${MASK.ipv4}`);
    expect(platformOf(WINDOWS_UA)).toBe("Windows");
    expect(platformOf("Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0)")).toBe("macOS");
    expect(platformOf("Mozilla/5.0 (X11; Linux x86_64)")).toBe("Linux");
    expect(platformOf("")).toBe("未知");
  });

  it("covers every kind the generated binding lists", () => {
    const source = readFileSync(new URL("./protocol/generated/LogEvent.ts", import.meta.url), "utf8");
    const kinds = [...source.matchAll(/\{ "kind": "([a-z_]+)"/g)].map((m) => m[1]!);
    expect(Object.keys(SAMPLES).sort()).toEqual(kinds.sort());
  });
});

describe("reading the whole log", () => {
  /// A watchdog with `total` lines (seq 1..total) that pages like the
  /// contract: newest first, at most MAX_EVENT_PAGE below `before`.
  function pager(total: number, failAt: number | null = null) {
    const calls: Array<number | null> = [];
    const read = async (before: number | null) => {
      calls.push(before);
      if (failAt !== null && calls.length === failAt) throw new Error("背景監控沒有回應");
      const top = before === null ? total : before - 1;
      const entries: EventEntry[] = [];
      for (let seq = top; seq >= 1 && entries.length < MAX_EVENT_PAGE; seq -= 1) {
        entries.push({ seq, at_unix_ms: T + seq, tunnel_id: null, removed_tunnel: false, event: { kind: "user_connect" } });
      }
      return { entries, more: top - entries.length >= 1 };
    };
    return { read, calls };
  }

  it("pages with before_seq until the watchdog has no more", async () => {
    const { read, calls } = pager(250);
    const seen: number[] = [];
    const log = await collectLog(read, EXPORT_CAP, (n) => seen.push(n));
    expect(log.entries).toHaveLength(250);
    expect(log.entries[0]!.seq).toBe(250);
    expect(log.entries[249]!.seq).toBe(1);
    expect(log).toMatchObject({ truncated: false, error: null });
    expect(calls).toEqual([null, 151, 51]);
    expect(seen).toEqual([100, 200, 250]);
  });

  it("stops at the cap and says the rest was left out", async () => {
    const { read, calls } = pager(10_000);
    const log = await collectLog(read, 250);
    expect(log.entries).toHaveLength(250);
    expect(log.truncated).toBe(true);
    expect(calls).toHaveLength(3);
    // Exactly at the cap with nothing older: not cut.
    const exact = await collectLog(pager(200).read, 200);
    expect(exact).toMatchObject({ truncated: false, error: null });
    expect(exact.entries).toHaveLength(200);
    // The default cap reads 50 pages.
    const big = pager(9_999);
    const capped = await collectLog(big.read);
    expect(capped.entries).toHaveLength(EXPORT_CAP);
    expect(big.calls).toHaveLength(EXPORT_CAP / MAX_EVENT_PAGE);
  });

  it("keeps what it read when a page fails, and never loops", async () => {
    const failing = await collectLog(pager(500, 3).read);
    expect(failing.entries).toHaveLength(200);
    expect(failing.error).toBe("背景監控沒有回應");
    const stuck = await collectLog(async () => ({
      entries: [{ seq: 5, at_unix_ms: T, tunnel_id: null, removed_tunnel: false, event: { kind: "user_connect" } }],
      more: true,
    }));
    expect(stuck.entries).toHaveLength(1);
    expect(stuck.error).toContain("沒有前進");
    const empty = await collectLog(async () => ({ entries: [], more: false }));
    expect(empty).toEqual({ entries: [], truncated: false, error: null });
  });
});
