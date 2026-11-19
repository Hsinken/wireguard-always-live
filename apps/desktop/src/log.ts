// The event log tab, without the DOM: one sentence for every kind of line
// the watchdog writes (`log_event.*`), which kind of event it is (the user's own, an
// automatic action, a fault, a recovery, something outside WGAL, or the
// watchdog's own), and the paging state of the newest-first list. The log
// only tells what happened; the snapshot alone says how things are now.

import { REFERENCE as R, t, table } from "./i18n";
import { offeredSince } from "./compat";
import type { EventEntry, LogEvent, Request, TunnelId } from "./protocol";
import { ADAPTER_ERROR, LIGHT, LIGHT_REASON, NOTICE_KIND, NOTIFICATION_FAILURE, TEST_STATE } from "./strings";
import { type Zone, formatCompact } from "./time";

export type LogKind = LogEvent["kind"];

/// Who or what an event comes from. Color is never the only cue: each has
/// a shape and a word.
export type LogCategory = "manual" | "auto" | "fault" | "ok" | "external" | "info";

const CATEGORY_ICON: Record<LogCategory, string> = {
  manual: "◆",
  auto: "↻",
  fault: "✕",
  ok: "✓",
  external: "⇄",
  info: "·",
};
const CATEGORY_LABEL = table("log_category", R.log_category satisfies Record<LogCategory, string>);

export const CATEGORY: Readonly<Record<LogCategory, { readonly icon: string; readonly label: string }>> =
  Object.freeze(
    Object.fromEntries(
      (Object.keys(CATEGORY_ICON) as LogCategory[]).map((c) => [
        c,
        Object.freeze({
          icon: CATEGORY_ICON[c],
          get label() {
            return CATEGORY_LABEL[c];
          },
        }),
      ]),
    ) as Record<LogCategory, { icon: string; label: string }>,
  );

/// The category of every kind; a Record, so `tsc` fails when the contract
/// gains a kind without one.
export const LOG_CATEGORY: Record<LogKind, LogCategory> = {
  watchdog_started: "info",
  watchdog_stopped: "info",
  monitoring_paused: "manual",
  monitoring_resumed: "manual",
  tunnel_registered: "manual",
  tunnel_removed: "manual",
  settings_changed: "manual",
  auto_recovery_changed: "manual",
  tunnel_notifications_changed: "manual",
  user_connect: "manual",
  user_disconnect: "manual",
  user_reconnect: "manual",
  probe_failed: "fault",
  probe_recovered: "ok",
  fault_confirmed: "fault",
  recovery_started: "auto",
  recovery_succeeded: "auto",
  recovery_failed: "fault",
  retries_exhausted: "fault",
  restarted_after_external_stop: "auto",
  control_failed: "fault",
  traffic_verified: "ok",
  light_changed: "info",
  stopped_externally: "external",
  started_externally: "external",
  local_network_offline: "fault",
  local_network_online: "ok",
  incident_closed: "ok",
  notification_sent: "info",
  notification_failed: "fault",
  notification_cancelled: "info",
  test_notification: "info",
  storage_failed: "fault",
  storage_recovered: "ok",
  lines_lost: "fault",
  agent_request: "manual",
};

/// Who a line is about when it names no registered tunnel: the whole host,
/// or a tunnel removed since.
export function hostLabel(): string {
  return t("log.host");
}

export function removedLabel(): string {
  return t("log.removed");
}

/// The sentence for one line. `host` says the line has no tunnel and is a
/// host event; `time` writes a time in the configured zone.
export function eventText(event: LogEvent, host: boolean, time: (unixMs: number) => string): string {
  switch (event.kind) {
    case "watchdog_started":
    case "watchdog_stopped":
    case "tunnel_registered":
    case "tunnel_removed":
    case "user_connect":
    case "user_disconnect":
    case "user_reconnect":
    case "recovery_succeeded":
    case "restarted_after_external_stop":
    case "traffic_verified":
    case "stopped_externally":
    case "started_externally":
    case "local_network_offline":
    case "local_network_online":
    case "storage_failed":
    case "storage_recovered":
      return t(`log_event.${event.kind}`);
    case "monitoring_paused":
      return t(host ? "log_event.monitoring_paused_host" : "log_event.monitoring_paused");
    case "monitoring_resumed":
      return t(host ? "log_event.monitoring_resumed_host" : "log_event.monitoring_resumed");
    case "settings_changed":
      return t(host ? "log_event.settings_changed_host" : "log_event.settings_changed");
    case "auto_recovery_changed":
      return t(event.enabled ? "log_event.auto_recovery_on" : "log_event.auto_recovery_off");
    case "tunnel_notifications_changed":
      return t(event.enabled ? "log_event.tunnel_notifications_on" : "log_event.tunnel_notifications_off");
    case "probe_failed":
      return t("log_event.probe_failed", { streak: event.streak, needed: event.needed });
    case "probe_recovered":
      return t("log_event.probe_recovered", { after: event.after });
    case "fault_confirmed":
      return t("log_event.fault_confirmed", { incident: event.incident_id });
    case "recovery_started":
      return t("log_event.recovery_started", { attempt: event.attempt });
    case "recovery_failed":
      return event.next_retry_unix_ms === null
        ? t("log_event.recovery_failed_no_retry")
        : t("log_event.recovery_failed", { at: time(event.next_retry_unix_ms) });
    case "retries_exhausted":
      return t("log_event.retries_exhausted", { retries: event.retries });
    case "control_failed":
      return t("log_event.control_failed", { error: ADAPTER_ERROR[event.error], code: event.error });
    case "light_changed":
      return t("log_event.light_changed", {
        icon: LIGHT[event.light].icon,
        light: LIGHT[event.light].label,
        reason: LIGHT_REASON[event.reason],
      });
    case "incident_closed":
      return t("log_event.incident_closed", { incident: event.incident_id });
    case "notification_sent":
      return t("log_event.notification_sent", { notice: NOTICE_KIND[event.notice], incident: event.incident_id });
    case "notification_failed":
      return t("log_event.notification_failed", {
        notice: NOTICE_KIND[event.notice],
        failure: NOTIFICATION_FAILURE[event.failure],
        incident: event.incident_id,
      });
    case "notification_cancelled":
      return t("log_event.notification_cancelled", { notice: NOTICE_KIND[event.notice], incident: event.incident_id });
    case "test_notification":
      return t("log_event.test_notification", { state: TEST_STATE[event.state] });
    case "agent_request":
      // The op's own name: what the agent asked for, as the contract
      // calls it.
      return t("log_event.agent_request", { op: event.op });
    case "lines_lost":
      return t("log_event.lines_lost", { count: event.count });
  }
}

export interface LogEntryView {
  seq: number;
  time: string;
  category: LogCategory;
  icon: string;
  categoryLabel: string;
  /// Which tunnel: its name, the host, or a removed tunnel.
  who: string;
  text: string;
}

/// One line as the log tab shows it. `names` maps registered tunnels to
/// their names in the snapshot; a tunnel not in it (none yet, or removed)
/// is named by its ID. Times leave out the year of `refMs` (the snapshot's
/// time), in the configured zone.
export function entryView(
  entry: EventEntry,
  names: ReadonlyMap<TunnelId, string>,
  zone: Zone | null,
  refMs: number,
): LogEntryView {
  // Without a tunnel ID a line is about the whole host, unless the watchdog
  // marks it as about a tunnel that has been removed since.
  const host = entry.tunnel_id === null && !entry.removed_tunnel;
  const who =
    entry.tunnel_id === null
      ? host
        ? hostLabel()
        : removedLabel()
      : (names.get(entry.tunnel_id) ?? entry.tunnel_id);
  const category = LOG_CATEGORY[entry.event.kind];
  return {
    seq: entry.seq,
    time: formatCompact(entry.at_unix_ms, refMs, zone),
    category,
    icon: CATEGORY[category].icon,
    categoryLabel: CATEGORY[category].label,
    who,
    text: eventText(entry.event, host, (ms) => formatCompact(ms, refMs, zone)),
  };
}

/// The lines loaded so far, newest first, and whether older ones exist.
export interface LogState {
  entries: EventEntry[];
  olderExist: boolean;
  /// Nothing has been loaded yet.
  empty: boolean;
}

export const NO_LOG: LogState = { entries: [], olderExist: false, empty: true };

/// Adds the newest page (`before_seq: null`). Lines already shown stay; the
/// new ones go on top. When the whole page is newer than everything shown
/// and the watchdog has more, lines in between may be missing, so the list
/// starts over from this page; it also does when the page is older than
/// what is shown (the watchdog's log was replaced).
export function withNewest(state: LogState, page: EventEntry[], more: boolean): LogState {
  const newest = state.entries[0]?.seq;
  if (state.empty || newest === undefined) {
    return { entries: page, olderExist: more, empty: false };
  }
  const top = page[0]?.seq;
  if (top === undefined) {
    return { entries: [], olderExist: false, empty: false };
  }
  if (top < newest) {
    return { entries: page, olderExist: more, empty: false };
  }
  const newer = page.filter((e) => e.seq > newest);
  if (newer.length === page.length && more) {
    return { entries: page, olderExist: more, empty: false };
  }
  return { entries: [...newer, ...state.entries], olderExist: state.olderExist, empty: false };
}

/// Adds a page read with `before_seq` = the oldest line shown.
export function withOlder(state: LogState, page: EventEntry[], more: boolean): LogState {
  const oldest = state.entries[state.entries.length - 1]?.seq;
  const older = oldest === undefined ? page : page.filter((e) => e.seq < oldest);
  return { entries: [...state.entries, ...older], olderExist: more, empty: false };
}

/// The `before_seq` that reads the page below what is shown.
export function olderThan(state: LogState): number | null {
  return state.entries[state.entries.length - 1]?.seq ?? null;
}

/// Which lines `ReadEventLog` returns (the contract's `EventFilter`, which
/// `src/protocol/index.ts` does not export by name).
export type LogFilter = Extract<Request, { op: "read_event_log" }>["filter"];

/// The filter select's values that are not a tunnel ID, which is only
/// letters, digits, `-` and `_`: the watchdog's link, and the lines about no
/// tunnel. Every line is the empty value.
export const WATCHDOG_OPTION = ":watchdog";
export const SYSTEM_OPTION = ":system";

/// The first watchdog version that reads the lines about no tunnel apart
/// (`{ kind: "system" }`); an older one refuses that filter.
export const SYSTEM_FILTER_SINCE = "1.1.0";

/// Whether the filter offers the lines about no tunnel, for the watchdog
/// version of the latest current snapshot: none known, or one that is not
/// plain `major.minor.patch`, is not offered.
export function systemFilterOffered(watchdogVersion: string | null): boolean {
  return offeredSince(watchdogVersion, SYSTEM_FILTER_SINCE);
}

/// Whether the filter offers the lines about no tunnel for the snapshot's
/// watchdog `version`, given the version that refused them as a malformed
/// request (`refusedBy`: the version the refused request was sent to, not
/// the one current when the answer came, Codex P3-N16), and what to
/// remember from now on: a refusal holds only while the snapshot names that
/// same version; any other version is judged afresh.
export function systemOffer(
  version: string | null,
  refusedBy: string | null,
): { offered: boolean; refusedBy: string | null } {
  const remembered = version !== null && version !== refusedBy ? null : refusedBy;
  return { offered: systemFilterOffered(version) && version !== remembered, refusedBy: remembered };
}

/// The filter a select value stands for.
export function logFilterOf(value: string): LogFilter {
  if (value === "") return { kind: "all" };
  if (value === WATCHDOG_OPTION) return { kind: "watchdog" };
  if (value === SYSTEM_OPTION) return { kind: "system" };
  return { kind: "tunnel", tunnel_id: value };
}

/// The select value that stands for a filter.
export function logFilterValue(filter: LogFilter): string {
  switch (filter.kind) {
    case "all":
      return "";
    case "watchdog":
      return WATCHDOG_OPTION;
    case "system":
      return SYSTEM_OPTION;
    case "tunnel":
      return filter.tunnel_id;
  }
}
