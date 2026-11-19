// Mirrors crates/wgal-protocol/tests/contract.rs. Each sample below is
// type-checked against the generated bindings by `tsc` and compared here with
// the JSON fixture the Rust test serializes, so a change on either side that
// the other does not follow fails one of the two suites.

import { readFileSync, readdirSync } from "node:fs";

import { describe, expect, it } from "vitest";

import {
  BOT_ID_DIGITS,
  BOT_SECRET_CHARS,
  CHAT_ID_DIGITS,
  CHAT_USERNAME_CHARS,
  DEFAULT_PROBE_TIMING,
  DEFAULT_RECOVERY_RETRY,
  DEVICE_NAME_MAX_CHARS,
  MAX_DISCOVERED,
  MAX_EVENT_PAGE,
  MAX_MESSAGE_BYTES,
  MAX_OBSERVE_TARGETS,
  MAX_TUNNELS,
  MAX_WORKERS,
  MIN_GUI_VERSION,
  MIN_USABLE_GUI_VERSION,
  MIN_WORKERS,
  PROBE_INTERVAL_SECS,
  PROBE_MIN_FAULT_SECS,
  PROBE_RETRIES,
  PROBE_TIMEOUT_SECS,
  PROTOCOL_VERSION,
  RECOVERY_RETRY_INTERVAL_SECS,
  RECOVERY_RETRY_LIMIT,
  SETTINGS_DOCUMENT_VERSION,
  TEMPLATE_PLACEHOLDERS,
  type ProbeTiming,
  type ProtocolInfo,
  type RequestEnvelope,
  type SettingsDocument,
  type ResponseEnvelope,
} from "./index";

const fixtures = new URL("../../../../crates/wgal-protocol/fixtures/", import.meta.url);

function fixture(name: string): unknown {
  return JSON.parse(readFileSync(new URL(`${name}.json`, fixtures), "utf8"));
}

const protocolInfo: ProtocolInfo = {
  protocol_version: PROTOCOL_VERSION,
  max_message_bytes: MAX_MESSAGE_BYTES,
};

// Built from the exported constants, so a limit that differs from the Rust
// one fails the fixture comparison below.
const limits = {
  max_tunnels: MAX_TUNNELS,
  max_discovered: MAX_DISCOVERED,
  max_observe_targets: MAX_OBSERVE_TARGETS,
  max_event_page: MAX_EVENT_PAGE,
  min_workers: MIN_WORKERS,
  max_workers: MAX_WORKERS,
  probe_interval_secs: PROBE_INTERVAL_SECS,
  probe_timeout_secs: PROBE_TIMEOUT_SECS,
  probe_retries: PROBE_RETRIES,
  probe_min_fault_secs: PROBE_MIN_FAULT_SECS,
  default_probe_timing: DEFAULT_PROBE_TIMING,
  recovery_retry_limit: RECOVERY_RETRY_LIMIT,
  recovery_retry_interval_secs: RECOVERY_RETRY_INTERVAL_SECS,
  default_recovery_retry: DEFAULT_RECOVERY_RETRY,
  min_gui_version: MIN_GUI_VERSION,
  min_usable_gui_version: MIN_USABLE_GUI_VERSION,
  template_placeholders: TEMPLATE_PLACEHOLDERS,
  bot_id_digits: BOT_ID_DIGITS,
  bot_secret_chars: BOT_SECRET_CHARS,
  chat_id_digits: CHAT_ID_DIGITS,
  chat_username_chars: CHAT_USERNAME_CHARS,
  device_name_max_chars: DEVICE_NAME_MAX_CHARS,
};

const slowTiming: ProbeTiming = { interval_secs: 30, timeout_secs: 3, retries: 1, min_fault_secs: 90 };

const requestGetSnapshot: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0001",
  request: { op: "get_snapshot" },
};

const requestSetAutoRecovery: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0002",
  request: { op: "set_auto_recovery", tunnel_id: "office-1", enabled: false },
};

const requestUpdateMonitorSettings: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0004",
  request: {
    op: "update_monitor_settings",
    tunnel_id: "office-1",
    settings: {
      recovery_target: { address: "192.0.2.1", probe: { kind: "icmp" } },
      observe_targets: [
        { address: "192.0.2.10", probe: { kind: "tcp_connect", port: 443 } },
        { address: "198.51.100.20", probe: { kind: "icmp" } },
      ],
      probe: slowTiming,
    },
  },
};

const requestUpdateNotificationSettings: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0006",
  request: {
    op: "update_notification_settings",
    enabled: true,
    device_name: "辦公室筆電",
    chat_id: "-1001234567890",
  },
};

const requestSetBotToken: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0007",
  request: { op: "set_bot_token", token: "123456789:EXAMPLE-not-a-real-token_0000000000" },
};

const requestSetMcpTelegramEnabled: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0021",
  request: { op: "set_mcp_telegram_enabled", enabled: true },
};

const requestSetCliLanguage: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0022",
  request: { op: "set_cli_language", language: "zh-TW" },
};

const requestClearCliLanguage: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0023",
  request: { op: "set_cli_language", language: null },
};

const requestSetMonitoringPaused: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0010",
  request: { op: "set_monitoring_paused", paused: true },
};

const requestSendTestNotification: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0009",
  request: { op: "send_test_notification" },
};

const requestSetTunnelMonitoring: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0011",
  request: { op: "set_tunnel_monitoring", tunnel_id: "lab-2", enabled: false },
};

const requestSetTunnelNotifications: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0012",
  request: { op: "set_tunnel_notifications", tunnel_id: "lab-2", enabled: false },
};

const requestSetRecoveryRetry: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0018",
  request: { op: "set_recovery_retry", tunnel_id: "office-1", retry: { limit: 100, interval_secs: 60 } },
};

// A settings file with one tunnel of each kind of entry.
const sampleDocument: SettingsDocument = {
  format: "wgal-settings",
  version: SETTINGS_DOCUMENT_VERSION,
  watchdog_version: "1.2.0",
  exported_at_unix_ms: 1_767_225_600_000,
  host: { workers: null, time_zone: "Asia/Taipei", notifications_enabled: true },
  tunnels: [
    {
      tunnel_id: "office-1",
      name: "Office",
      monitoring: true,
      notifications: true,
      auto_recovery: true,
      start_when_stopped_externally: false,
      recovery_target: { address: "192.0.2.1", probe: { kind: "icmp" } },
      observe_targets: [{ address: "198.51.100.7", probe: { kind: "tcp_connect", port: 443 } }],
      probe: DEFAULT_PROBE_TIMING,
      retry: DEFAULT_RECOVERY_RETRY,
    },
    {
      tunnel_id: "lab-2",
      name: "實驗室",
      monitoring: false,
      notifications: false,
      auto_recovery: false,
      start_when_stopped_externally: true,
      recovery_target: null,
      observe_targets: [],
      probe: slowTiming,
      retry: { limit: null, interval_secs: 300 },
    },
  ],
};

const requestExportSettings: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0019",
  request: { op: "export_settings" },
};

const responseSettingsExport: ResponseEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0019",
  outcome: { ok: { kind: "settings_export", document: sampleDocument } },
};

const requestImportSettings: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0020",
  request: {
    op: "import_settings",
    document: sampleDocument,
    options: { match_by: "name", register_missing: true, monitor_new: false, dry_run: true },
  },
};

const responseSettingsImport: ResponseEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0020",
  outcome: {
    ok: {
      kind: "settings_import",
      result: {
        dry_run: true,
        host: { kind: "applied" },
        notifications: { kind: "applied" },
        tunnels: [
          {
            index: 0,
            name: "Office",
            tunnel_id: "office-1",
            outcome: { kind: "registered" },
            auto_recovery: { kind: "waiting_for_baseline" },
          },
          { index: 1, name: "實驗室", tunnel_id: null, outcome: { kind: "not_found" }, auto_recovery: { kind: "off" } },
        ],
      },
    },
  },
};

const requestUnregisterTunnel: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0013",
  request: { op: "unregister_tunnel", tunnel_id: "lab-2" },
};

const requestUpdateHostSettings: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0014",
  request: { op: "update_host_settings", settings: { workers: 3, time_zone: "Asia/Taipei" } },
};

const requestReadEventLog: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0015",
  request: { op: "read_event_log", filter: { kind: "tunnel", tunnel_id: "home-3" }, before_seq: 1042 },
};

// The watchdog's built-in templates, which the GUI's zh-TW language file
// must match text for text; the ID is an example.
const requestSetNotificationTemplates: RequestEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0017",
  request: {
    op: "set_notification_templates",
    templates: {
      id: "en/92c719a8",
      formal:
        "[WG Always Live] Fault alert\n" +
        "Device: {device}\n" +
        "Tunnel: {tunnel}\n" +
        "Incident: {incident}\n" +
        "Time zone: {time_zone}\n" +
        "Fault started: {started_at}\n" +
        "Recovery result: {result}\n" +
        "{retry}",
      result_failed: "The first automatic recovery failed (still no traffic after restarting the tunnel)",
      result_not_executed: "Automatic recovery could not run (the tunnel cannot be started or stopped)",
      retry_limited: "WG Always Live will try again every {interval} seconds, up to {limit} times; you will be notified again once traffic is back and stable for 2 minutes.",
      retry_forever: "WG Always Live will keep trying every {interval} seconds; you will be notified again once traffic is back and stable for 2 minutes.",
      retry_none: "WG Always Live will not retry automatically; please handle it manually. You will be notified again once traffic is back and stable for 2 minutes.",
      exhausted:
        "[WG Always Live] Retries stopped\n" +
        "Device: {device}\n" +
        "Tunnel: {tunnel}\n" +
        "Incident: {incident}\n" +
        "Time zone: {time_zone}\n" +
        "Fault started: {started_at}\n" +
        "Retried automatically {retries} times and reached the limit; automatic recovery has stopped. Please handle it manually; you will be notified again once traffic is back and stable for 2 minutes.",
      recovered:
        "[WG Always Live] Recovered\n" +
        "Device: {device}\n" +
        "Tunnel: {tunnel}\n" +
        "Incident: {incident}\n" +
        "Time zone: {time_zone}\n" +
        "Fault started: {started_at}\n" +
        "Recovered at: {recovered_at}\n" +
        "Traffic is back and has been stable for 2 minutes.",
      test: "[WG Always Live] Test notification\nThis is a test message and does not mean any VPN fault.\nDevice: {device}\nTime zone: {time_zone}\nTime: {time}",
      unnamed_device: "(unnamed device)",
    },
  },
};

const responseSnapshot: ResponseEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0001",
  outcome: {
    ok: {
      kind: "snapshot",
      snapshot: {
        generated_at_unix_ms: 1_767_225_600_000,
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
          workers: { configured: null, suggested: 4, limit: 4, effective: 4, running: 3 },
          time_zone: { configured: "Asia/Taipei", effective: "Asia/Taipei", utc_offset_secs: 28_800 },
        },
        notification: {
          enabled: true,
          device_name: "辦公室筆電",
          chat_id: "-1001234567890",
          token: "stored",
          last_test: {
            request_id: "req-0009",
            state: "accepted",
            requested_at_unix_ms: 1_767_225_500_000,
            finished_at_unix_ms: 1_767_225_501_200,
            failure: null,
          },
          last_failure: null,
          templates_id: "zh-TW/3f2a91c0",
          cli_language: "zh-TW",
          host_name: "office-pc",
        },
        tunnels: [
          {
            tunnel_id: "office-1",
            display_name: "Office",
            light: "green",
            light_reason: "verified",
            light_since_unix_ms: 1_767_225_540_000,
            deadline_unix_ms: null,
            next_retry_unix_ms: null,
            last_verified_unix_ms: 1_767_225_599_000,
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
                started_at_unix_ms: 1_767_225_598_980,
                finished_at_unix_ms: 1_767_225_599_000,
                result: { result: "reply", rtt_ms: 12 },
              },
            },
            observe_targets: [
              {
                target: { address: "192.0.2.10", probe: { kind: "tcp_connect", port: 443 } },
                last_probe: {
                  started_at_unix_ms: 1_767_225_599_080,
                  finished_at_unix_ms: 1_767_225_599_100,
                  result: { result: "no_reply" },
                },
              },
            ],
            probe: DEFAULT_PROBE_TIMING,
            freshness_secs: 35,
            consecutive_failures: 0,
            fault: null,
            incident: null,
            retry: DEFAULT_RECOVERY_RETRY,
            recovered_notification: null,
            auto_recovery_pending: false,
          },
          {
            tunnel_id: "lab-2",
            display_name: "實驗室",
            light: "red",
            light_reason: "probe_unavailable",
            light_since_unix_ms: 1_767_225_480_000,
            deadline_unix_ms: null,
            next_retry_unix_ms: null,
            last_verified_unix_ms: null,
            intent: {
              desired: "connected",
              monitoring_enabled: true,
              auto_recovery_enabled: false,
              start_when_stopped_externally: false,
            },
            auto_recovery_allowed: false,
            notifications_enabled: false,
            takeover: "observe_only",
            os_state: "connected",
            data_health: "unknown",
            recovery: "blocked",
            recovery_target: {
              target: { address: "192.0.2.2", probe: { kind: "tcp_connect", port: 443 } },
              last_probe: {
                started_at_unix_ms: 1_767_225_597_980,
                finished_at_unix_ms: 1_767_225_598_000,
                result: { result: "error", error: "permission_denied" },
              },
            },
            observe_targets: [],
            probe: slowTiming,
            freshness_secs: 51,
            consecutive_failures: 0,
            fault: { kind: "permission_denied", message: "probe tool not permitted" },
            incident: null,
            retry: { limit: null, interval_secs: 300 },
            recovered_notification: null,
            auto_recovery_pending: false,
          },
          {
            tunnel_id: "home-3",
            display_name: "Home",
            light: "red",
            light_reason: "recovery_failed",
            light_since_unix_ms: 1_767_225_420_000,
            deadline_unix_ms: null,
            next_retry_unix_ms: 1_767_225_660_000,
            last_verified_unix_ms: 1_767_225_100_000,
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
            data_health: "failed",
            recovery: "backoff",
            recovery_target: {
              target: { address: "203.0.113.1", probe: { kind: "icmp" } },
              last_probe: {
                started_at_unix_ms: 1_767_225_596_980,
                finished_at_unix_ms: 1_767_225_597_000,
                result: { result: "no_reply" },
              },
            },
            observe_targets: [],
            probe: DEFAULT_PROBE_TIMING,
            freshness_secs: 35,
            consecutive_failures: 9,
            fault: null,
            incident: {
              incident_id: "inc-0007",
              opened_at_unix_ms: 1_767_225_300_000,
              first_recovery: "failed",
              backoff_step: 1,
              stable_since_unix_ms: null,
              notification: "sent",
              retries_used: 1,
              retries_exhausted: false,
              exhausted_notification: "not_eligible",
            },
            retry: { limit: 5, interval_secs: 120 },
            recovered_notification: { incident_id: "inc-0006", notification: "sent" },
            auto_recovery_pending: false,
          },
        ],
      },
    },
  },
};

const responseDiscovered: ResponseEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0005",
  outcome: {
    ok: {
      kind: "discovered",
      tunnels: [
        { tunnel_id: "office-1", display_name: "Office", control: "supported" },
        { tunnel_id: "lab-2", display_name: "實驗室", control: "supported" },
        { tunnel_id: "home-3", display_name: "Home (NetworkManager)", control: "unsupported" },
      ],
    },
  },
};

const responseEventLog: ResponseEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0016",
  outcome: {
    ok: {
      kind: "event_log",
      entries: [
        {
          seq: 1042,
          at_unix_ms: 1_767_225_970_000,
          tunnel_id: "home-3",
          removed_tunnel: false,
          event: { kind: "retries_exhausted", retries: 5 },
        },
        {
          seq: 1041,
          at_unix_ms: 1_767_225_361_500,
          tunnel_id: "home-3",
          removed_tunnel: false,
          event: { kind: "notification_sent", incident_id: "inc-0007", notice: "alert" },
        },
        {
          seq: 1040,
          at_unix_ms: 1_767_225_360_000,
          tunnel_id: "home-3",
          removed_tunnel: false,
          event: { kind: "recovery_failed", next_retry_unix_ms: 1_767_225_420_000 },
        },
        {
          seq: 1039,
          at_unix_ms: 1_767_225_360_000,
          tunnel_id: "home-3",
          removed_tunnel: false,
          event: { kind: "light_changed", light: "red", reason: "recovery_failed" },
        },
        {
          seq: 1038,
          at_unix_ms: 1_767_225_300_000,
          tunnel_id: "home-3",
          removed_tunnel: false,
          event: { kind: "recovery_started", attempt: 1 },
        },
        {
          seq: 1037,
          at_unix_ms: 1_767_225_300_000,
          tunnel_id: "home-3",
          removed_tunnel: false,
          event: { kind: "fault_confirmed", incident_id: "inc-0007" },
        },
        {
          seq: 1036,
          at_unix_ms: 1_767_225_300_000,
          tunnel_id: "home-3",
          removed_tunnel: false,
          event: { kind: "probe_failed", streak: 5, needed: 3 },
        },
        {
          seq: 1035,
          at_unix_ms: 1_767_225_010_000,
          tunnel_id: null,
          removed_tunnel: true,
          event: { kind: "tunnel_removed" },
        },
        {
          seq: 1034,
          at_unix_ms: 1_767_225_000_000,
          tunnel_id: null,
          removed_tunnel: false,
          event: { kind: "watchdog_started" },
        },
      ],
      more: true,
    },
  },
};

const responseError: ResponseEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: "req-0003",
  outcome: { err: { code: "unknown_tunnel", reason: "tunnel_not_registered", message: "tunnel is not registered" } },
};

const responseMessageTooLarge: ResponseEnvelope = {
  protocol_version: PROTOCOL_VERSION,
  request_id: null,
  outcome: { err: { code: "message_too_large", reason: null, message: "request exceeds 65536 bytes" } },
};

const samples: Record<string, unknown> = {
  protocol_info: protocolInfo,
  limits,
  request_get_snapshot: requestGetSnapshot,
  request_set_auto_recovery: requestSetAutoRecovery,
  request_update_monitor_settings: requestUpdateMonitorSettings,
  request_update_notification_settings: requestUpdateNotificationSettings,
  request_set_bot_token: requestSetBotToken,
  request_send_test_notification: requestSendTestNotification,
  request_set_monitoring_paused: requestSetMonitoringPaused,
  request_set_mcp_telegram_enabled: requestSetMcpTelegramEnabled,
  request_set_cli_language: requestSetCliLanguage,
  request_clear_cli_language: requestClearCliLanguage,
  request_set_tunnel_monitoring: requestSetTunnelMonitoring,
  request_set_tunnel_notifications: requestSetTunnelNotifications,
  request_set_recovery_retry: requestSetRecoveryRetry,
  request_export_settings: requestExportSettings,
  request_import_settings: requestImportSettings,
  response_settings_export: responseSettingsExport,
  response_settings_import: responseSettingsImport,
  request_unregister_tunnel: requestUnregisterTunnel,
  request_update_host_settings: requestUpdateHostSettings,
  request_read_event_log: requestReadEventLog,
  request_set_notification_templates: requestSetNotificationTemplates,
  response_snapshot: responseSnapshot,
  response_discovered: responseDiscovered,
  response_event_log: responseEventLog,
  response_error: responseError,
  response_message_too_large: responseMessageTooLarge,
};

describe("protocol contract fixtures", () => {
  it.each(Object.entries(samples))("%s.json matches the typed sample", (name, sample) => {
    expect(fixture(name)).toStrictEqual(sample);
  });

  it("has a typed sample for every fixture", () => {
    const names = readdirSync(fixtures)
      .filter((file) => file.endsWith(".json"))
      .map((file) => file.slice(0, -".json".length));
    expect(names.sort()).toEqual(Object.keys(samples).sort());
  });
});
