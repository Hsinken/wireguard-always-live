// A made-up watchdog for looking at the window in a plain browser during
// development (`pnpm dev`, then open http://127.0.0.1:1420/?tunnels=16).
// `main.ts` loads it only when `import.meta.env.DEV` is true and the page is
// not inside the desktop shell, so the production build never contains it.
// Every name and address here is invented; addresses are documentation
// ranges. It answers like the contract but decides nothing real.
//
// Query options: tunnels=N (0-16, default 5), paused=1, storage=failed,
// lost=1 (no answers), silentAfter=S (stops answering S seconds after
// loading) and backAfter=S (answers again S seconds after loading),
// staleAfter=S (stops renewing the green tunnels' evidence S seconds after
// loading, so the window shows their lights as expired; the real watchdog
// would change the light too),
// failAs=<not_running|refused|not_the_watchdog|timeout|hang> (how it does
// not answer: the shell's refusal of that kind, or nothing at all; by
// default the request throws), zone=<IANA name>; versions: guiver= (the window's, 1.0.0),
// wdver= (the watchdog's, 1.0.0), mingui= (the oldest window the watchdog
// works with, 1.0.0); templatesId=<id> (the message templates the
// watchdog reports in use, null by default: its built-in set) and
// templates=refuse (it refuses SetNotificationTemplates as storage
// unavailable); exportAs=refused (saving an export fails); for
// screenshots, tab=<tunnels|log|notify|settings|about> selects a tab and
// open=N opens card N's settings.

import type { WatchdogClient } from "../client";
import type { SavedExport } from "../exportlog";
import {
  DEFAULT_PROBE_TIMING,
  DEFAULT_RECOVERY_RETRY,
  MAX_EVENT_PAGE,
  type DiscoveredTunnel,
  type ErrorReason,
  type EventEntry,
  type LightReason,
  type LogEvent,
  type Outcome,
  type Request,
  type Snapshot,
  type TunnelStatus,
} from "../protocol";
import { freshnessSecs } from "../timing";

const NAMES = [
  "辦公室 VPN",
  "Lab-02",
  "家用 NAS 備援",
  "客戶甲（台北機房）",
  "Office-VPN-Office-VPN-Office-VPN-Office-VPN-Office-VPN-Office-VPN-Office-VPN-long",
  "實驗室",
  "Branch-07",
  "倉庫",
  "測試站",
  "Backup-Link",
  "總部",
  "遠端桌面",
  "研發",
  "會計",
  "Dev-Box",
  "第十六條隧道",
];

type Kind = [TunnelStatus["light"], LightReason, Partial<TunnelStatus>];

function kinds(now: number): Kind[] {
  return [
    ["green", "verified", {}],
    ["red", "recovery_failed", { next_retry_unix_ms: now + 95_000, recovery: "backoff", data_health: "failed", consecutive_failures: 9 }],
    ["yellow", "confirming", { deadline_unix_ms: now + 40_000, data_health: "suspect", consecutive_failures: 2 }],
    ["gray", "disconnected_by_user", { os_state: "disconnected" }],
    ["green", "verified", {}],
    ["gray", "monitoring_off", {}],
    ["red", "fault_confirmed", { data_health: "failed", consecutive_failures: 4 }],
    ["yellow", "verifying", { deadline_unix_ms: now + 30_000, recovery: "verifying" }],
    ["gray", "not_configured", { recovery_target: null }],
    ["red", "local_network_offline", {}],
    ["green", "verified", {}],
    ["yellow", "grace_period", { deadline_unix_ms: now + 20_000 }],
    ["red", "stopped_externally", { takeover: "stopped_externally", os_state: "disconnected" }],
    ["green", "verified", {}],
    ["red", "binding_lost", { takeover: "binding_lost", os_state: "unknown" }],
    ["green", "verified", {}],
  ];
}

/// The recovery target answered 4 s before `now`.
function renewEvidence(t: TunnelStatus, now: number): void {
  t.last_verified_unix_ms = now - 4_000;
  if (t.recovery_target?.last_probe) {
    t.recovery_target.last_probe.started_at_unix_ms = now - 4_020;
    t.recovery_target.last_probe.finished_at_unix_ms = now - 4_000;
  }
}

function tunnel(i: number, now: number): TunnelStatus {
  const [light, reason, over] = kinds(now)[i % 16]!;
  const id = `fake-${String(i + 1).padStart(2, "0")}`;
  const base: TunnelStatus = {
    tunnel_id: id,
    display_name: NAMES[i % NAMES.length]!,
    light,
    light_reason: reason,
    light_since_unix_ms: now - 300_000,
    deadline_unix_ms: null,
    next_retry_unix_ms: null,
    last_verified_unix_ms: light === "green" ? now - 4_000 : now - 3_600_000,
    intent: {
      desired: reason === "disconnected_by_user" ? "disconnected" : "connected",
      monitoring_enabled: reason !== "monitoring_off",
      auto_recovery_enabled: i % 3 !== 2,
      start_when_stopped_externally: false,
    },
    auto_recovery_allowed: i % 4 !== 3,
    notifications_enabled: i % 5 !== 4,
    takeover: "managed",
    os_state: "connected",
    data_health: light === "green" ? "healthy" : "unknown",
    recovery: "idle",
    recovery_target: {
      target: { address: `192.0.2.${i + 1}`, probe: i % 2 === 0 ? { kind: "icmp" } : { kind: "tcp_connect", port: 22 } },
      last_probe: {
        started_at_unix_ms: now - 4_020,
        finished_at_unix_ms: now - 4_000,
        result: light === "red" ? { result: "no_reply" } : { result: "reply", rtt_ms: 8 + i },
      },
    },
    observe_targets:
      i % 3 === 0
        ? [
            {
              target: { address: `198.51.100.${i + 1}`, probe: { kind: "tcp_connect", port: 445 } },
              last_probe: { started_at_unix_ms: now - 5_000, finished_at_unix_ms: now - 4_900, result: { result: "no_reply" } },
            },
            { target: { address: `198.51.100.${i + 101}`, probe: { kind: "icmp" } }, last_probe: null },
          ]
        : [],
    probe: i === 1 ? { interval_secs: 30, timeout_secs: 3, retries: 1, min_fault_secs: 90 } : { ...DEFAULT_PROBE_TIMING },
    freshness_secs: 0,
    consecutive_failures: 0,
    fault: light === "red" ? { kind: "timeout", message: "probe of the recovery target timed out" } : null,
    incident:
      reason === "recovery_failed"
        ? {
            incident_id: "inc-000042",
            opened_at_unix_ms: now - 900_000,
            first_recovery: "failed",
            backoff_step: 2,
            stable_since_unix_ms: null,
            notification: "sent",
            retries_used: 2,
            retries_exhausted: false,
            exhausted_notification: "not_eligible",
          }
        : null,
    recovered_notification: null,
    auto_recovery_pending: false,
    retry: i === 1 ? { limit: null, interval_secs: 300 } : { ...DEFAULT_RECOVERY_RETRY },
  };
  const made = { ...base, ...over };
  // As the watchdog reports it, observe targets included.
  made.freshness_secs = freshnessSecs(made.probe, made.observe_targets.length);
  return made;
}

const LOG_SAMPLES: LogEvent[] = [
  { kind: "watchdog_started" },
  { kind: "tunnel_registered" },
  { kind: "settings_changed" },
  { kind: "traffic_verified" },
  { kind: "user_disconnect" },
  { kind: "user_connect" },
  { kind: "probe_failed", streak: 1, needed: 3 },
  { kind: "probe_failed", streak: 2, needed: 3 },
  { kind: "probe_recovered", after: 2 },
  { kind: "probe_failed", streak: 3, needed: 3 },
  { kind: "fault_confirmed", incident_id: "inc-000042" },
  { kind: "recovery_started", attempt: 1 },
  { kind: "recovery_failed", next_retry_unix_ms: 0 },
  { kind: "notification_sent", incident_id: "inc-000042", notice: "alert" },
  { kind: "recovery_started", attempt: 2 },
  { kind: "recovery_succeeded" },
  { kind: "incident_closed", incident_id: "inc-000042" },
  { kind: "stopped_externally" },
  { kind: "restarted_after_external_stop" },
  { kind: "local_network_offline" },
  { kind: "local_network_online" },
  { kind: "light_changed", light: "yellow", reason: "confirming" },
  { kind: "auto_recovery_changed", enabled: false },
  { kind: "tunnel_notifications_changed", enabled: true },
  { kind: "monitoring_paused" },
  { kind: "monitoring_resumed" },
  { kind: "user_reconnect" },
  { kind: "control_failed", error: "permission_denied" },
  { kind: "test_notification", state: "accepted" },
  { kind: "storage_failed" },
  { kind: "storage_recovered" },
  // Started again without a stop in between: shown as 上次未正常停止.
  { kind: "watchdog_started" },
  { kind: "lines_lost", count: 3 },
  { kind: "started_externally" },
  { kind: "notification_failed", incident_id: "inc-000041", failure: "network_unreachable", notice: "alert" },
  { kind: "notification_cancelled", incident_id: "inc-000040", notice: "alert" },
  { kind: "tunnel_removed" },
  { kind: "watchdog_stopped" },
];

const HOST_ONLY = new Set(["watchdog_started", "watchdog_stopped", "storage_failed", "storage_recovered", "lines_lost", "local_network_offline", "local_network_online", "test_notification"]);

export class FakeClient implements WatchdogClient {
  private readonly lost: boolean;
  private readonly silentAt: number;
  private readonly backAt: number;
  private readonly staleAt: number;
  private readonly failAs: string | null;
  private readonly templatesRefused: boolean;
  private snapshot: Snapshot;
  private log: EventEntry[] = [];
  private seq = 0;

  constructor(query: string) {
    const params = new URLSearchParams(query);
    const now = Date.now();
    const n = Math.max(0, Math.min(16, Number(params.get("tunnels") ?? "5") || 0));
    this.lost = params.get("lost") === "1";
    const tab = params.get("tab");
    if (tab !== null) {
      try {
        window.localStorage.setItem("wgal.tab", tab);
      } catch {
        // Only a convenience for screenshots.
      }
    }
    const open = params.get("open");
    if (open !== null) {
      setTimeout(() => {
        const card = document.querySelectorAll("#tunnels .card")[Number(open)];
        const details = card?.querySelector("details.settings");
        const main = document.querySelector("main");
        if (details instanceof HTMLDetailsElement && main) {
          details.open = true;
          main.scrollTop += details.getBoundingClientRect().top - main.getBoundingClientRect().top - 8;
        }
      }, 400);
    }
    this.silentAt = params.has("silentAfter") ? now + Number(params.get("silentAfter")) * 1000 : Infinity;
    this.backAt = params.has("backAfter") ? now + Number(params.get("backAfter")) * 1000 : Infinity;
    this.staleAt = params.has("staleAfter") ? now + Number(params.get("staleAfter")) * 1000 : Infinity;
    this.failAs = params.get("failAs");
    this.templatesRefused = params.get("templates") === "refuse";
    const zone = params.get("zone") ?? "Asia/Taipei";
    this.snapshot = {
      generated_at_unix_ms: now,
      storage: params.get("storage") === "failed" ? "failed" : "ok",
      capabilities: {
        query_state: "supported",
        start_stop: "supported",
        bound_icmp_probe: "supported",
        bound_tcp_probe: "supported",
        read_metrics: "unsupported",
        verified_concurrent_tunnels: 2,
        background_operation: "supported",
      },
      monitoring_paused: params.get("paused") === "1",
      host: {
        watchdog_version: params.get("wdver") ?? "1.0.0",
        min_gui_version: params.get("mingui") ?? "1.0.0",
        min_usable_gui_version: params.get("minusable") ?? params.get("mingui") ?? "1.0.0",
        mcp_enabled: params.get("mcp") === "1",
        mcp_telegram_enabled: params.get("mcptelegram") === "1",
        workers: { configured: null, suggested: Math.min(4, n + 1), limit: Math.max(2, n + 1), effective: Math.min(4, n + 1), running: 2 },
        time_zone: { configured: params.has("zone") ? zone : null, effective: zone, utc_offset_secs: 28_800 },
      },
      notification: {
        enabled: true,
        device_name: "辦公室筆電",
        chat_id: "-1001234567890",
        token: "stored",
        last_test: {
          request_id: "gui-test",
          state: "accepted",
          requested_at_unix_ms: now - 600_000,
          finished_at_unix_ms: now - 599_000,
          failure: null,
        },
        last_failure: null,
        templates_id: params.get("templatesId"),
        cli_language: params.get("cliLanguage"),
        host_name: params.get("hostName"),
      },
      tunnels: Array.from({ length: n }, (_, i) => tunnel(i, now)),
    };
    // Three days of lines, oldest first, so paging has something to read.
    const ids = this.snapshot.tunnels.map((t) => t.tunnel_id);
    for (let k = 0; k < 260; k += 1) {
      const event = LOG_SAMPLES[k % LOG_SAMPLES.length]!;
      const at = now - (260 - k) * 900_000;
      const hostOnly = HOST_ONLY.has(event.kind) || ids.length === 0;
      const fixed = event.kind === "recovery_failed" ? { ...event, next_retry_unix_ms: at + 120_000 } : event;
      this.write(fixed, hostOnly ? null : (ids[k % ids.length] ?? null), at);
    }
  }

  private write(event: LogEvent, tunnelId: string | null, at = Date.now()): void {
    this.seq += 1;
    this.log.push({ seq: this.seq, at_unix_ms: at, tunnel_id: tunnelId, removed_tunnel: false, event });
  }

  private find(id: string): TunnelStatus | undefined {
    return this.snapshot.tunnels.find((t) => t.tunnel_id === id);
  }

  /// Stands in for the shell's `app_version`: `guiver=` in the query, else
  /// 1.0.0. With `wdver=` and `mingui=` the watchdog reports other versions,
  /// to see the 關於 tab's verdicts.
  async appVersion(): Promise<string> {
    return new URLSearchParams(window.location.search).get("guiver") ?? "1.0.0";
  }

  /// Stands in for the shell's `mcp_version`: `mcpver=` in the query is the
  /// version, `missing` or `unreadable`; else the watchdog's (`wdver=`).
  async mcpVersion(): Promise<unknown> {
    const query = new URLSearchParams(window.location.search);
    const mcp = query.get("mcpver");
    if (mcp === "missing" || mcp === "unreadable") return { state: mcp };
    return { state: "installed", version: mcp ?? query.get("wdver") ?? "1.0.0" };
  }

  /// Stands in for the shell's `mcp_clients`: every tool, Codex already
  /// set up, VS Code without a settings folder, Gemini CLI unreadable.
  async mcpClients(): Promise<unknown> {
    const command = String.raw`C:\Program Files\WGAL\wgal-mcp.exe`;
    const views = ["claude_code", "claude_desktop", "cursor", "vs_code", "trae", "kiro", "codex", "antigravity", "gemini_cli"].map(
      (client) => {
        const path = String.raw`C:\Users\you\.${client}\mcp.json`;
        const state =
          this.setUp.has(client) || (client === "codex" && !this.removed.has(client))
            ? "configured"
            : client === "vs_code"
              ? "no_folder"
              : client === "gemini_cli"
                ? "unreadable"
                : "not_configured";
        const preview = JSON.stringify({ mcpServers: { wgal: { command, args: [] } } }, null, 2);
        return { client, path, state, preview };
      },
    );
    return views;
  }

  /// Stands in for the shell's `set_up_mcp_client`: writes nothing; Cursor
  /// refuses as if the file had just changed.
  async setUpMcpClient(client: string): Promise<unknown> {
    await new Promise((r) => setTimeout(r, 200));
    if (client === "cursor") throw { reason: "changed" };
    this.setUp.add(client);
    const views = (await this.mcpClients()) as Array<{ client: string }>;
    return views.find((v) => v.client === client);
  }

  /// Stands in for the shell's `remove_mcp_client`: writes nothing; a
  /// tool without the entry refuses as the shell does.
  async removeMcpClient(client: string): Promise<unknown> {
    await new Promise((r) => setTimeout(r, 200));
    const views = (await this.mcpClients()) as Array<{ client: string; state: string }>;
    const view = views.find((v) => v.client === client);
    if (view === undefined || (view.state !== "configured" && view.state !== "other")) throw { reason: "not_there" };
    this.setUp.delete(client);
    this.removed.add(client);
    return (await this.mcpClients() as Array<{ client: string }>).find((v) => v.client === client);
  }

  private readonly setUp = new Set<string>();
  private readonly removed = new Set<string>();

  /// Stands in for the shell's `save_log_export`: keeps the text on
  /// `globalThis.wgalLastExport` for a look in the console and writes
  /// nothing. With `exportAs=refused` it refuses as the shell does when it
  /// cannot create the file.
  async saveSettings(deviceName: string, utcOffsetMinutes: number): Promise<SavedExport> {
    await new Promise((r) => setTimeout(r, 200));
    const local = new Date(Date.now() + utcOffsetMinutes * 60_000).toISOString();
    const stamp = local.slice(0, 19).replace(/[-:]/g, "").replace("T", "-");
    const part = deviceName.trim().replace(/\s+/g, "-");
    return { folder: "downloads", name: `wgal-settings-${part}-${stamp}.json（開發用假背景監控：沒有寫入檔案）` };
  }

  async saveExport(text: string, utcOffsetMinutes: number): Promise<SavedExport> {
    await new Promise((r) => setTimeout(r, 200));
    (globalThis as Record<string, unknown>).wgalLastExport = text;
    const local = new Date(Date.now() + utcOffsetMinutes * 60_000).toISOString();
    const stamp = local.slice(0, 19).replace(/[-:]/g, "").replace("T", "-");
    if (new URLSearchParams(window.location.search).get("exportAs") === "refused") {
      throw { reason: "cannot_create", detail: "PermissionDenied" };
    }
    return { folder: "downloads", name: `wgal-log-${stamp}.txt（開發用假背景監控：沒有寫入檔案）` };
  }

  /// The answer while it is "gone": the shell's refusal of the kind asked
  /// for, no answer at all, or a throw.
  private async fail(): Promise<Outcome> {
    const refuse = (code: "internal" | "unauthorized" | "timeout", reason: ErrorReason, message: string): Outcome => ({
      err: { code, reason, message },
    });
    switch (this.failAs) {
      case "not_running":
        return refuse("internal", "not_running", "the background watchdog service is not installed or not running");
      case "not_the_watchdog":
        return refuse("internal", "not_the_watchdog", "the pipe is not served by the WGAL service; nothing was sent to it");
      case "refused":
        return refuse("unauthorized", "caller_not_allowed", "this account is not on the installation's access list");
      case "timeout":
        return refuse("timeout", "no_answer_in_time", "the background watchdog did not answer in time");
      case "hang":
        return new Promise<Outcome>(() => undefined);
      default:
        throw new Error("（開發用假背景監控）沒有回應");
    }
  }

  async request(_requestId: string, request: Request): Promise<Outcome> {
    await new Promise((r) => setTimeout(r, 120));
    const now = Date.now();
    if (this.lost || (now > this.silentAt && now < this.backAt)) return this.fail();
    const ok = (): Outcome => ({ ok: { kind: "accepted" } });
    const err = (
      code: "unknown_tunnel" | "busy" | "invalid_setting" | "invalid_state" | "storage_unavailable",
      reason: ErrorReason | null,
      message: string,
    ): Outcome => ({
      err: { code, reason, message },
    });
    const s = this.snapshot;
    switch (request.op) {
      case "get_snapshot":
        s.generated_at_unix_ms = Date.now();
        // As the watchdog's probe rounds do: a green light's evidence is a
        // few seconds old whenever it is asked.
        if (now < this.staleAt) {
          for (const t of s.tunnels) if (t.light === "green") renewEvidence(t, now);
        }
        return { ok: { kind: "snapshot", snapshot: structuredClone(s) } };
      case "discover_tunnels": {
        // `?discover=none`: as on a machine without the official WireGuard
        // app or any tunnel it runs.
        if (new URLSearchParams(window.location.search).get("discover") === "none") {
          return { ok: { kind: "discovered", tunnels: [] } };
        }
        const found: DiscoveredTunnel[] = s.tunnels.map((t) => ({ tunnel_id: t.tunnel_id, display_name: t.display_name, control: "supported" }));
        found.push(
          { tunnel_id: "fake-new-1", display_name: "新的隧道", control: "supported" },
          { tunnel_id: "fake-nm-1", display_name: "NetworkManager 管理的", control: "unsupported" },
        );
        return { ok: { kind: "discovered", tunnels: found } };
      }
      case "register_tunnel": {
        if (!this.find(request.tunnel_id)) {
          const t = tunnel(s.tunnels.length, Date.now());
          s.tunnels.push({ ...t, tunnel_id: request.tunnel_id, display_name: "新的隧道", light: "gray", light_reason: "not_configured", recovery_target: null });
        }
        this.write({ kind: "tunnel_registered" }, request.tunnel_id);
        return ok();
      }
      case "read_event_log": {
        const f = request.filter;
        const keeps = (e: EventEntry): boolean => {
          switch (f.kind) {
            case "all":
              return true;
            case "tunnel":
              return e.tunnel_id === f.tunnel_id;
            case "watchdog":
              return e.tunnel_id === null && !e.removed_tunnel && (e.event.kind === "watchdog_started" || e.event.kind === "watchdog_stopped");
            case "system":
              // Every line about no tunnel; a removed tunnel's is about one.
              return e.tunnel_id === null && !e.removed_tunnel;
          }
        };
        const pool = this.log.filter(keeps);
        const below = pool.filter((e) => request.before_seq === null || e.seq < request.before_seq).reverse();
        return { ok: { kind: "event_log", entries: below.slice(0, MAX_EVENT_PAGE), more: below.length > MAX_EVENT_PAGE } };
      }
      case "update_notification_settings":
        s.notification = { ...s.notification, enabled: request.enabled, device_name: request.device_name, chat_id: request.chat_id };
        return ok();
      case "set_bot_token":
        s.notification.token = "stored";
        return ok();
      case "clear_bot_token":
        s.notification.token = "not_set";
        return ok();
      case "set_monitoring_paused":
        s.monitoring_paused = request.paused;
        this.write({ kind: request.paused ? "monitoring_paused" : "monitoring_resumed" }, null);
        return ok();
      case "send_test_notification":
        s.notification.last_test = { request_id: _requestId, state: "accepted", requested_at_unix_ms: Date.now(), finished_at_unix_ms: Date.now() + 800, failure: null };
        this.write({ kind: "test_notification", state: "accepted" }, null);
        return ok();
      case "set_notification_templates":
        if (this.templatesRefused) return err("storage_unavailable", "storage_save_failed", "cannot store the templates");
        s.notification.templates_id = request.templates.id;
        return ok();
      case "set_cli_language":
        // wgal's shared language; the window neither shows nor sets it.
        s.notification.cli_language = request.language;
        return ok();
      case "update_host_settings": {
        const zone = request.settings.time_zone;
        if (zone !== null && !Intl.supportedValuesOf("timeZone").includes(zone) && zone !== "UTC") {
          return err("invalid_setting", "unknown_time_zone", `unknown time zone ${zone}`);
        }
        s.host.workers.configured = request.settings.workers;
        s.host.time_zone = { configured: zone, effective: zone ?? "Asia/Taipei", utc_offset_secs: 28_800 };
        this.write({ kind: "settings_changed" }, null);
        return ok();
      }
      case "set_mcp_enabled":
        s.host.mcp_enabled = request.enabled;
        this.write({ kind: "settings_changed" }, null);
        return ok();
      case "set_mcp_telegram_enabled":
        s.host.mcp_telegram_enabled = request.enabled;
        this.write({ kind: "settings_changed" }, null);
        return ok();
      case "export_settings":
        return {
          ok: {
            kind: "settings_export",
            document: {
              format: "wgal-settings",
              version: 1,
              watchdog_version: s.host.watchdog_version,
              exported_at_unix_ms: Date.now(),
              host: {
                workers: s.host.workers.configured,
                time_zone: s.host.time_zone.configured,
                notifications_enabled: s.notification.enabled,
              },
              tunnels: s.tunnels.map((t) => ({
                tunnel_id: t.tunnel_id,
                name: t.display_name,
                monitoring: t.intent.monitoring_enabled,
                notifications: t.notifications_enabled,
                auto_recovery: t.intent.auto_recovery_enabled,
                start_when_stopped_externally: t.intent.start_when_stopped_externally,
                recovery_target: t.recovery_target?.target ?? null,
                observe_targets: t.observe_targets.map((o) => o.target),
                probe: t.probe,
                retry: t.retry,
              })),
            },
          },
        };
      case "import_settings": {
        // As the watchdog would, without changing anything here.
        const by = request.options.match_by;
        const tunnels = request.document.tunnels.map((entry, index) => {
          const found = s.tunnels.filter((t) => (by === "name" ? t.display_name === entry.name : t.tunnel_id === entry.tunnel_id));
          const one = found.length === 1 ? found[0]! : null;
          return {
            index,
            name: entry.name,
            tunnel_id: one?.tunnel_id ?? null,
            outcome: one !== null ? { kind: "applied" as const } : found.length > 1 ? { kind: "ambiguous" as const } : { kind: "not_found" as const },
            auto_recovery: entry.auto_recovery ? { kind: "on" as const } : { kind: "off" as const },
          };
        });
        return { ok: { kind: "settings_import", result: { dry_run: request.options.dry_run, host: { kind: "applied" }, notifications: { kind: "applied" }, tunnels } } };
      }
      default:
        break;
    }
    const t = this.find(request.tunnel_id);
    if (!t) return err("unknown_tunnel", "tunnel_not_registered", "no such tunnel");
    switch (request.op) {
      case "unregister_tunnel":
        if (t.recovery !== "idle") return err("busy", "operation_in_progress", "an operation on this tunnel is still running");
        s.tunnels = s.tunnels.filter((x) => x !== t);
        this.write({ kind: "tunnel_removed" }, null);
        return ok();
      case "update_monitor_settings":
        t.recovery_target = request.settings.recovery_target && { target: request.settings.recovery_target, last_probe: null };
        t.observe_targets = request.settings.observe_targets.map((target) => ({ target, last_probe: null }));
        t.probe = request.settings.probe;
        t.freshness_secs = freshnessSecs(t.probe, t.observe_targets.length);
        this.write({ kind: "settings_changed" }, t.tunnel_id);
        return ok();
      case "set_tunnel_monitoring":
        t.intent.monitoring_enabled = request.enabled;
        t.light = request.enabled ? "yellow" : "gray";
        t.light_reason = request.enabled ? "grace_period" : "monitoring_off";
        t.deadline_unix_ms = request.enabled ? Date.now() + 30_000 : null;
        this.write({ kind: request.enabled ? "monitoring_resumed" : "monitoring_paused" }, t.tunnel_id);
        return ok();
      case "set_auto_recovery":
        if (request.enabled && !t.auto_recovery_allowed) return err("invalid_state", "baseline_not_passed", "baseline not passed");
        t.intent.auto_recovery_enabled = request.enabled;
        this.write({ kind: "auto_recovery_changed", enabled: request.enabled }, t.tunnel_id);
        return ok();
      case "set_tunnel_notifications":
        t.notifications_enabled = request.enabled;
        this.write({ kind: "tunnel_notifications_changed", enabled: request.enabled }, t.tunnel_id);
        return ok();
      case "set_start_when_stopped_externally":
        t.intent.start_when_stopped_externally = request.enabled;
        return ok();
      case "set_recovery_retry":
        t.retry = request.retry;
        if (t.incident !== null && (request.retry.limit === null || t.incident.retries_used < request.retry.limit)) {
          t.incident.retries_exhausted = false;
        }
        this.write({ kind: "settings_changed" }, t.tunnel_id);
        return ok();
      case "connect":
      case "reconnect":
        t.intent.desired = "connected";
        t.os_state = "connected";
        t.light = "green";
        t.light_reason = "verified";
        t.deadline_unix_ms = null;
        this.write({ kind: request.op === "connect" ? "user_connect" : "user_reconnect" }, t.tunnel_id);
        return ok();
      case "disconnect":
        t.intent.desired = "disconnected";
        t.os_state = "disconnected";
        t.light = "gray";
        t.light_reason = "disconnected_by_user";
        t.deadline_unix_ms = null;
        t.next_retry_unix_ms = null;
        this.write({ kind: "user_disconnect" }, t.tunnel_id);
        return ok();
      case "probe_now":
        return ok();
    }
  }
}
