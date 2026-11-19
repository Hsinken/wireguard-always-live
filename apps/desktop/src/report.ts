// The log export (plan §9.3, owner's request of 2026-11-03), without the
// DOM: a plain UTF-8 text a person can read and a tool can parse, built
// from the snapshot and the event log the GUI already gets. Tunnels are
// numbered in snapshot order (`report.tunnel_n`) unless the user asks for
// their names; target addresses are always masked; every other string from
// the watchdog, names and tunnel IDs included, goes through `scrub` last.
// Nothing the GUI never holds (the bot token, the chat ID, configs, `wg`
// output, the environment, system logs) can be in it.

import { compatibility, compatibleText, guiNeedsText, watchdogNeedsText } from "./compat";
import { t } from "./i18n";
import { CATEGORY, LOG_CATEGORY, eventText, hostLabel, removedLabel } from "./log";
import {
  MIN_USABLE_WATCHDOG_VERSION,
  MIN_WATCHDOG_VERSION,
  PROTOCOL_VERSION,
  type EventEntry,
  type LogEvent,
  type Snapshot,
  type TargetStatus,
  type TunnelId,
  type TunnelStatus,
} from "./protocol";
import { MASK, scrub } from "./scrub";
import {
  ADAPTER_ERROR,
  CAPABILITY,
  DATA_HEALTH,
  FIRST_RECOVERY,
  LIGHT,
  LIGHT_REASON,
  NOTIFICATION_STATE,
  OS_STATE,
  RECOVERY_PHASE,
  TAKEOVER,
} from "./strings";
import { type Zone, formatWithOffset, zoneOf, zoneText } from "./time";
import { failuresNeeded, judgmentText } from "./timing";
import { CATEGORY_OF_LINK, type LinkRecord, guiLabel, linkFields, linkText } from "./watchlog";

/// Most event log lines one export reads (50 pages of `MAX_EVENT_PAGE`).
export const EXPORT_CAP = 5_000;

/// What target addresses become in every export.
export function addressMask(): string {
  return MASK.address;
}

/// How a tunnel is called in one export: numbered by its place in the
/// snapshot, or its name when the user asked for names. A name is masked
/// like any other string from the watchdog, and so is the tunnel ID added
/// to a name another tunnel has too; where masking leaves two labels alike
/// (two UUIDs, two addresses), each also gets its number. A line about a
/// tunnel no longer in the snapshot gets the next free number, the same one
/// for every line about it; removed tunnels and host lines get their labels.
export class ExportNames {
  private readonly names = new Map<TunnelId, string>();
  private next = 1;

  constructor(tunnels: readonly TunnelStatus[], private readonly includeNames: boolean) {
    const count = (keys: readonly string[]) => {
      const seen = new Map<string, number>();
      for (const k of keys) seen.set(k, (seen.get(k) ?? 0) + 1);
      return (k: string) => seen.get(k) ?? 0;
    };
    const sameName = count(tunnels.map((s) => s.display_name));
    const own = tunnels.map((s) =>
      sameName(s.display_name) > 1
        ? t("common.name_with_id", { name: scrub(s.display_name), id: scrub(s.tunnel_id) })
        : scrub(s.display_name),
    );
    const alike = count(own);
    tunnels.forEach((s, i) => {
      const anonymous = t("report.tunnel_n", { n: this.next++ });
      const label = own[i] ?? anonymous;
      const named = alike(label) > 1 ? t("common.name_with_id", { name: label, id: anonymous }) : label;
      if (!this.names.has(s.tunnel_id)) this.names.set(s.tunnel_id, includeNames ? named : anonymous);
    });
  }

  /// The tunnel's name in this export.
  tunnel(id: TunnelId): string {
    let name = this.names.get(id);
    if (name === undefined) {
      name = t("report.tunnel_n_gone", { n: this.next++ });
      this.names.set(id, name);
    }
    return name;
  }

  /// Who a log line is about.
  entry(entry: EventEntry): string {
    if (entry.tunnel_id !== null) return this.tunnel(entry.tunnel_id);
    return entry.removed_tunnel ? removedLabel() : hostLabel();
  }

  get withNames(): boolean {
    return this.includeNames;
  }
}

export interface ReportInput {
  snapshot: Snapshot | null;
  /// The window does not hear from the watchdog now.
  lost: boolean;
  /// Why, when it said why.
  lastError: string | null;
  /// Event log lines, newest first.
  entries: EventEntry[];
  /// Reading stopped at `EXPORT_CAP` with older lines left.
  truncated: boolean;
  /// Reading the log failed (after `entries`, if any).
  logError: string | null;
  includeNames: boolean;
  nowMs: number;
  /// The desktop binary's version, null when the shell did not say.
  guiVersion: string | null;
  /// The window's user agent, of which only the OS family is kept.
  userAgent: string;
  /// What the window saw of its link to the watchdog, newest first, and
  /// whether those records outlive the window (`watchlog.ts`).
  linkRecords: readonly LinkRecord[];
  linkStored: boolean;
}

/// "Windows", "macOS", "Linux", or unknown (`report.platform_unknown`).
export function platformOf(userAgent: string): string {
  if (/Windows/i.test(userAgent)) return "Windows";
  if (/Macintosh|Mac OS X/i.test(userAgent)) return "macOS";
  if (/Linux|X11/i.test(userAgent)) return "Linux";
  return t("report.platform_unknown");
}

const onOff = (on: boolean) => (on ? "on" : "off");

/// A log event's fields as `kind=probe_failed streak=2 needed=3`, strings
/// scrubbed. The tunnel ID is never among them.
export function rawFields(event: LogEvent): string {
  const parts = [`kind=${event.kind}`];
  for (const [key, value] of Object.entries(event)) {
    if (key === "kind") continue;
    parts.push(`${key}=${typeof value === "string" ? scrub(value) : String(value)}`);
  }
  return parts.join(" ");
}

function targetLine(label: string, target: TargetStatus, zone: Zone | null): string {
  const probe = target.target.probe;
  const parts = [
    `address=${addressMask()}`,
    probe.kind === "icmp" ? "probe=icmp" : `probe=tcp_connect port=${probe.port}`,
  ];
  const last = target.last_probe;
  if (last === null) {
    parts.push("last=none");
  } else {
    const r = last.result;
    parts.push(`last=${r.result}`);
    if (r.result === "reply" || r.result === "refused") parts.push(`rtt=${r.rtt_ms}ms`);
    if (r.result === "error") parts.push(`error=${r.error}`);
    parts.push(
      `started=${formatWithOffset(last.started_at_unix_ms, zone)}`,
      `finished=${formatWithOffset(last.finished_at_unix_ms, zone)}`,
    );
  }
  return t("common.labelled", { label, text: parts.join(" ") });
}

function tunnelLines(s: TunnelStatus, name: string, zone: Zone | null): string[] {
  const time = (ms: number | null) => (ms === null ? "—" : formatWithOffset(ms, zone));
  const p = s.probe;
  const intent = [
    `desired=${s.intent.desired}`,
    `monitoring=${onOff(s.intent.monitoring_enabled)}`,
    `auto_recovery=${onOff(s.intent.auto_recovery_enabled)}`,
    `start_when_stopped_externally=${onOff(s.intent.start_when_stopped_externally)}`,
    `notifications=${onOff(s.notifications_enabled)}`,
    `auto_recovery_allowed=${onOff(s.auto_recovery_allowed)}`,
  ].join(" ");
  const probe = `interval=${p.interval_secs}s timeout=${p.timeout_secs}s retries=${p.retries} min_fault=${p.min_fault_secs}s`;
  const lines = [
    `[${name}]`,
    t("report.light", {
      light: s.light,
      light_text: LIGHT[s.light].label,
      reason: s.light_reason,
      reason_text: LIGHT_REASON[s.light_reason],
      since: time(s.light_since_unix_ms),
    }),
    t("report.os_state", { code: s.os_state, text: OS_STATE[s.os_state] }),
    t("report.data_health", { code: s.data_health, text: DATA_HEALTH[s.data_health] }),
    t("report.takeover", { code: s.takeover, text: TAKEOVER[s.takeover] }),
    t("report.intent", { fields: intent }),
    t("report.probe", { fields: probe, judgment: judgmentText(p) }),
    t("report.failures", { count: s.consecutive_failures, needed: failuresNeeded(p) }),
    t("report.freshness", { secs: s.freshness_secs }),
    t("report.last_verified", { at: time(s.last_verified_unix_ms) }),
    t("report.deadline", { deadline: time(s.deadline_unix_ms), retry: time(s.next_retry_unix_ms) }),
    t("report.recovery", { code: s.recovery, text: RECOVERY_PHASE[s.recovery] }),
    s.fault === null
      ? t("report.fault_none")
      : t("report.fault", { code: s.fault.kind, text: ADAPTER_ERROR[s.fault.kind], message: scrub(s.fault.message) }),
  ];
  const i = s.incident;
  lines.push(
    i === null
      ? t("report.incident_none")
      : t("report.incident", {
          id: scrub(i.incident_id),
          opened: time(i.opened_at_unix_ms),
          first: i.first_recovery,
          first_text: FIRST_RECOVERY[i.first_recovery],
          step: i.backoff_step,
          stable: time(i.stable_since_unix_ms),
          notification: i.notification,
          notification_text: NOTIFICATION_STATE[i.notification],
        }),
  );
  lines.push(
    s.recovery_target === null
      ? t("report.recovery_target_none")
      : targetLine(t("card.recovery_target"), s.recovery_target, zone),
  );
  s.observe_targets.forEach((o, k) => lines.push(targetLine(t("report.observe_n", { n: k + 1 }), o, zone)));
  return lines;
}

/// Whether the window and the watchdog work together, by the snapshot the
/// export has (the last one while lost, which the line then says).
function compatibilityLine(input: ReportInput): string {
  const s = input.snapshot;
  if (s === null) return t("report.compat_no_snapshot");
  // Judged on the versions themselves; only the text below says them.
  const c = compatibility(
    input.guiVersion,
    s.host.watchdog_version,
    s.host.min_gui_version,
    s.host.min_usable_gui_version,
  );
  const last = input.lost ? t("report.compat_from_last") : "";
  switch (c.kind) {
    case "compatible":
      return t("report.compat", { verdict: compatibleText(), last });
    case "incompatible":
      return t("report.compat", { verdict: scrub(c.problems.join(t("compat.separator"))), last });
    case "unknown":
      return t("report.compat_no_gui");
  }
}

function headerLines(input: ReportInput, names: ExportNames, zone: Zone | null): string[] {
  const s = input.snapshot;
  const tz = s?.host.time_zone;
  const fromLast = input.lost ? t("report.last_snapshot") : "";
  const lost = input.lastError === null ? t("report.lost") : t("report.lost_because", { error: scrub(input.lastError) });
  const watchdog =
    s === null
      ? t("report.watchdog_none", { lost })
      : input.lost
        ? t("report.watchdog_last", { lost, at: formatWithOffset(s.generated_at_unix_ms, zone) })
        : t("report.watchdog_ok", { at: formatWithOffset(s.generated_at_unix_ms, zone) });
  const lines = [
    t("report.title"),
    t("report.exported_at", { at: formatWithOffset(input.nowMs, zone) }),
    t("report.gui_version", { version: input.guiVersion ?? t("about.unavailable") }),
    t("report.watchdog_version", {
      version: s === null ? t("report.no_snapshot") : `${scrub(s.host.watchdog_version)}${fromLast}`,
    }),
    t("report.gui_needs", { value: guiNeedsText(MIN_WATCHDOG_VERSION, MIN_USABLE_WATCHDOG_VERSION) }),
    t("report.watchdog_needs", {
      value:
        s === null
          ? t("report.no_snapshot")
          : `${watchdogNeedsText(
              scrub(s.host.min_gui_version),
              s.host.min_usable_gui_version === null ? null : scrub(s.host.min_usable_gui_version),
            )}${fromLast}`,
    }),
    compatibilityLine(input),
    t("report.protocol", { version: PROTOCOL_VERSION }),
    t("report.platform", { platform: platformOf(input.userAgent) }),
    tz === undefined
      ? t("report.zone_unknown")
      : t("report.zone", {
          configured: tz.configured === null ? t("settings.zone_follow") : scrub(tz.configured),
          zone: scrub(zoneText(tz)),
        }),
    t("report.watchdog", { state: watchdog }),
  ];
  if (s !== null) {
    const w = s.host.workers;
    const c = s.capabilities;
    const capabilities = [
      `query_state=${c.query_state}`,
      `start_stop=${c.start_stop}`,
      `bound_icmp_probe=${c.bound_icmp_probe}`,
      `bound_tcp_probe=${c.bound_tcp_probe}`,
      `read_metrics=${c.read_metrics}`,
      `verified_concurrent_tunnels=${c.verified_concurrent_tunnels}`,
      `background_operation=${c.background_operation}`,
    ].join(" ");
    lines.push(
      t("report.storage", {
        state: s.storage === "ok" ? t("report.storage_ok") : t("report.storage_failed"),
        code: s.storage,
      }),
      t("report.monitoring", {
        state: s.monitoring_paused ? t("report.monitoring_paused") : t("report.monitoring_running"),
      }),
      t("report.workers", {
        configured: w.configured === null ? t("report.workers_auto") : w.configured,
        suggested: w.suggested,
        limit: w.limit,
        effective: w.effective,
        running: w.running,
      }),
      t("report.capabilities", { fields: capabilities, text: CAPABILITY[c.background_operation] }),
      t("report.telegram", { state: s.notification.enabled ? t("report.on") : t("report.off") }),
    );
  }
  lines.push(names.withNames ? t("report.names_included") : t("report.names_anonymous"), t("report.masking"));
  return lines;
}

function logLines(input: ReportInput, names: ExportNames, zone: Zone | null): string[] {
  const time = (ms: number) => formatWithOffset(ms, zone);
  const lines = [t("report.log_title", { count: input.entries.length })];
  if (input.truncated) lines.push(t("report.log_truncated", { cap: EXPORT_CAP }));
  if (input.logError !== null) lines.push(t("report.log_error", { error: scrub(input.logError) }));
  if (input.entries.length === 0) lines.push(t("report.empty"));
  for (const entry of input.entries) {
    const host = entry.tunnel_id === null && !entry.removed_tunnel;
    const category = CATEGORY[LOG_CATEGORY[entry.event.kind]];
    lines.push(
      [
        time(entry.at_unix_ms),
        names.entry(entry),
        category.label,
        scrub(eventText(entry.event, host, time)),
        `seq=${entry.seq} ${entry.removed_tunnel ? "removed_tunnel=true " : ""}${rawFields(entry.event)}`,
      ].join(" | "),
    );
  }
  return lines;
}

/// The window's own records: kinds and times only, never an error's text.
function linkLines(input: ReportInput, zone: Zone | null): string[] {
  const time = (ms: number) => formatWithOffset(ms, zone);
  const lines = [
    t("report.link_title", { count: input.linkRecords.length }),
    input.linkStored ? t("report.link_stored") : t("report.link_not_stored"),
  ];
  if (input.linkRecords.length === 0) lines.push(t("report.empty"));
  for (const r of input.linkRecords) {
    lines.push(
      [time(r.atMs), guiLabel(), CATEGORY_OF_LINK.label, linkText(r.event, time), `id=${r.id} ${linkFields(r.event)}`].join(
        " | ",
      ),
    );
  }
  return lines;
}

/// The whole export.
export function buildReport(input: ReportInput): string {
  const zone = zoneOf(input.snapshot?.host.time_zone);
  const tunnels = input.snapshot?.tunnels ?? [];
  const names = new ExportNames(tunnels, input.includeNames);
  const lines = [...headerLines(input, names, zone), ""];
  lines.push(t("report.tunnels_title", { count: tunnels.length }));
  if (tunnels.length === 0) lines.push(t("report.no_tunnels"));
  for (const s of tunnels) lines.push(...tunnelLines(s, names.tunnel(s.tunnel_id), zone), "");
  lines.push(...logLines(input, names, zone), "", ...linkLines(input, zone), "", t("report.end"), "");
  return lines.join("\n");
}

/// One page of the event log: read with `before_seq`, newest first.
export type PageReader = (beforeSeq: number | null) => Promise<{ entries: EventEntry[]; more: boolean }>;

/// Reads the event log newest first, page by page, until the watchdog has
/// no more or `cap` lines are read. A failed page ends the reading with
/// what was read so far and the error.
export async function collectLog(
  read: PageReader,
  cap: number = EXPORT_CAP,
  progress: (count: number) => void = () => undefined,
): Promise<{ entries: EventEntry[]; truncated: boolean; error: string | null }> {
  const entries: EventEntry[] = [];
  let before: number | null = null;
  for (;;) {
    let page: { entries: EventEntry[]; more: boolean };
    try {
      page = await read(before);
    } catch (e) {
      return { entries, truncated: false, error: e instanceof Error ? e.message : String(e) };
    }
    const fresh = before === null ? page.entries : page.entries.filter((e) => e.seq < (before as number));
    entries.push(...fresh);
    progress(Math.min(entries.length, cap));
    if (entries.length >= cap) {
      const truncated = entries.length > cap || page.more;
      return { entries: entries.slice(0, cap), truncated, error: null };
    }
    const oldest = entries[entries.length - 1]?.seq;
    if (!page.more || oldest === undefined) {
      return { entries, truncated: false, error: null };
    }
    if (fresh.length === 0) {
      // "More" but nothing older: reading on would never end.
      return { entries, truncated: false, error: t("report.paging_stuck") };
    }
    before = oldest;
  }
}
