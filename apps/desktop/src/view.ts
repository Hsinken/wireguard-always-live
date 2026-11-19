// Snapshot -> what the window and the tray show. Pure functions only: the
// watchdog decides every light, reason, deadline and retry time (plan §6.1);
// this module only words them and adds the two things the GUI itself knows:
// whether it still hears from the watchdog (plan §6.2: heartbeat 5 s, lost
// after 15 s), and whether a green light it still shows has outlived its
// evidence (plan §6.2: no stale green), which a long refresh interval makes
// possible between two snapshots. The pieces of state, `LatestOnly` and
// `ExpiryWatch`, only order snapshot answers and say when to ask for one;
// they decide nothing about a tunnel.

import {
  MAX_DISCOVERED,
  MAX_TUNNELS,
  type DesiredState,
  type DiscoveredTunnel,
  type Light,
  type ProbeResult,
  type ProbeTarget,
  type ProbeTiming,
  type ProtocolError,
  type RecoveryRetry,
  type Request,
  type Snapshot,
  type TargetStatus,
  type TunnelId,
  type TunnelStatus,
} from "./protocol";
import { RECOVERY_RETRY_SINCE, offeredSince } from "./compat";
import { faultSaid } from "./errors";
import { around, t } from "./i18n";
import { type Said, type Words, plain } from "./said";
import { blankName } from "./templates";
import {
  CREDENTIAL,
  DATA_HEALTH,
  ERROR_CODE,
  FIRST_RECOVERY,
  LIGHT,
  LIGHT_REASON,
  NOTIFICATION_FAILURE,
  NOTIFICATION_STATE,
  OS_STATE,
  RECOVERY_PHASE,
  TAKEOVER,
  TEST_STATE,
  ADAPTER_ERROR,
} from "./strings";
import { type Zone, formatDateTime, formatShort, zoneOf } from "./time";
import { failureProgress, judgmentShort, judgmentText, timingSummary } from "./timing";

/// After how long without a fresh snapshot the GUI shows the watchdog as
/// lost with the default refresh interval (plan §6.2: 15 s). A longer
/// interval lengthens it (`prefs.ts`, `lostAfterMs`).
export const LOST_AFTER_MS = 15_000;

/// What the GUI knows about its link to the watchdog. `receivedAtMono` is
/// `performance.now()` when the last snapshot arrived: monotonic, so a wall
/// clock change cannot make an old snapshot look fresh. `lastError` words
/// why the last request for one failed, in the language of the moment.
/// `lostAfterMs` is the lost-link threshold for the window's refresh
/// interval, 15 s when not given. `ageOnArrivalMs` is how old the snapshot
/// was when it arrived, fixed then (`arrivalAgeMs`), 0 when not given.
export interface Link {
  snapshot: Snapshot | null;
  receivedAtMono: number | null;
  lastError: Words | null;
  lostAfterMs?: number;
  ageOnArrivalMs?: number;
}

/// Orders the answers to snapshot requests, which overlap when the watchdog
/// is slow (the bridge waits up to 10 s, the heartbeat is 5 s). `issue()`
/// numbers a request when it is sent; `accept(n)` is true only for an answer
/// to a newer request than every answer used so far, so a late snapshot or a
/// late error never replaces newer news.
export class LatestOnly {
  private issued = 0;
  private applied = 0;

  issue(): number {
    this.issued += 1;
    return this.issued;
  }

  accept(n: number): boolean {
    if (n <= this.applied) return false;
    this.applied = n;
    return true;
  }
}

export type Availability = { kind: "enabled" } | { kind: "disabled"; why: string };

export interface TargetView {
  label: string;
  result: string;
  /// The last probe failed, however old it is.
  failing: boolean;
  /// The last result is news (see `isCurrent`).
  current: boolean;
  /// When the last probe finished, if there was one.
  atMs: number | null;
  /// Why a result is shown but not counted: "" when it is current or there
  /// is none.
  note: string;
}

/// A switch on the card: its state, whether it can be flipped now, and a
/// note shown under it ("" for none).
export interface SwitchView {
  on: boolean;
  toggle: Availability;
  note: string;
}

/// What the settings form of a card is filled with when it is opened: the
/// watchdog's current values, as the fields show them.
export interface SettingsView {
  recoveryAddress: string;
  recoveryPort: string;
  observeText: string;
  probe: ProbeTiming;
  retry: RecoveryRetry;
  /// The watchdog has a recovery retry per tunnel (1.2.0 and later); an
  /// older one's `retry` is only the contract's default, never shown.
  retrySupported: boolean;
}

export interface RowView {
  id: TunnelId;
  name: string;
  /// The name as shown and in every control's accessible name: the display
  /// name, with the tunnel ID added when another row has the same name, so
  /// rows never read alike.
  label: string;
  /// Snapshot time, for the "updated at" stamps.
  updatedMs: number;
  light: Light | "unknown";
  icon: string;
  lightLabel: string;
  reason: string;
  /// Shown over the card instead of a green light whose evidence is older
  /// than its freshness by now (see `greenExpired`); null otherwise.
  expired: string | null;
  /// The tunnel's fault in words, its original text in the details; null
  /// when there is none or the watchdog is lost.
  fault: Said | null;
  /// Disconnected on purpose: shown as intended, not as a problem.
  intended: boolean;
  sinceMs: number;
  deadlineMs: number | null;
  nextRetryMs: number | null;
  /// The streak of failed rounds while one runs (`timing.failures`), else
  /// null.
  failures: string | null;
  target: string;
  latency: string;
  lastVerifiedMs: number | null;
  osState: string;
  desired: DesiredState;
  monitoring: SwitchView;
  autoRecovery: SwitchView;
  notifications: SwitchView;
  startWhenStoppedExternally: SwitchView;
  connect: Availability;
  disconnect: Availability;
  probeNow: Availability;
  reconnect: Availability;
  /// The settings form: it changes settings, so not while the watchdog is
  /// lost.
  edit: Availability;
  unregister: Availability;
  settings: SettingsView;
  /// The current timing in one line, and the judgment time it gives.
  timing: string;
  judgment: string;
  judgmentShort: string;
  /// How old a result may be and still count (`freshness_secs`).
  freshnessSecs: number;
  details: Array<[string, string]>;
  observe: TargetView[];
  /// Observe targets down now: failing, and current.
  hostIssues: number;
  /// One line about the observe targets, with its label, and without it for
  /// the card's own row.
  observeSummary: string;
  observeShort: string;
}

export interface Overlay {
  title: string;
  text: string;
  /// The original text of the error, for the details.
  detail: string | null;
}

/// The dot on the tray icon (owner's request 12): green when every
/// monitored tunnel is fine, red when one is in a fault, none otherwise.
export type TrayDot = "none" | "green" | "red";

export interface TraySummary {
  /// Worst state worth an icon: lost link, a VPN fault, a green whose
  /// evidence has expired, a host target down, paused, or fine. A host
  /// target down never shows as a VPN fault, and an expired green is never
  /// fine.
  level: "lost" | "fault" | "expired" | "host" | "paused" | "ok";
  vpnFaults: number;
  hostIssues: number;
  monitor: "ok" | "paused" | "lost" | "storage_failed";
  dot: TrayDot;
  /// How long from now the green dot, and the words that go with it, may
  /// stay without a newer summary (`trayGreenForMs`); 0 when the dot is not
  /// green. The shell holds the dot to it on its own clock, so a hidden
  /// window whose timers stall cannot keep a green its evidence no longer
  /// supports.
  greenForMs: number;
  tooltip: string;
}

export interface ViewModel {
  overlay: Overlay | null;
  paused: boolean;
  banner: string | null;
  rows: RowView[];
  tray: TraySummary;
  /// The whole list in one line, for the header that stays in view.
  summary: string;
  /// The same line in parts, each light's count in that light's colour
  /// (`SummaryPart`); their texts joined are `summary`.
  summaryParts: readonly SummaryPart[];
  /// The watchdog's state for the header, with when the window last heard
  /// from it.
  link: { monitor: TraySummary["monitor"]; text: string };
  /// The zone every time is shown in (see `time.ts`).
  zone: Zone | null;
}

export function isLost(link: Link, nowMono: number): boolean {
  return (
    link.snapshot === null ||
    link.receivedAtMono === null ||
    link.lastError !== null ||
    nowMono - link.receivedAtMono > (link.lostAfterMs ?? LOST_AFTER_MS)
  );
}

/// The shape a green light whose evidence expired shows instead of "●".
const EXPIRED_ICON = "◷";

/// How old a snapshot was when it arrived, fixed once then: the longer of
/// the monotonic time since its request was sent (`sentMono`; the watchdog
/// made it after that, so it is no older) and its age by the local wall
/// clock (`wallAtArrivalMs`, the same machine's clock the watchdog stamps
/// with), never below 0. Only the monotonic clock ages it afterwards
/// (`watchdogNowMs`), so a wall clock set back later never makes it younger
/// again and a deadline given for its evidence only ever shrinks.
export function arrivalAgeMs(
  sentMono: number,
  receivedMono: number,
  generatedAtMs: number,
  wallAtArrivalMs: number,
): number {
  return Math.max(0, receivedMono - sentMono, wallAtArrivalMs - generatedAtMs);
}

/// The watchdog's clock now, as far as the window can tell: the snapshot's
/// time plus its age when it arrived (`Link.ageOnArrivalMs`) plus the
/// monotonic time since. No wall clock is read here, so setting it back or
/// forward after the snapshot arrived changes nothing. Null without a
/// snapshot.
export function watchdogNowMs(link: Link, nowMono: number): number | null {
  if (link.snapshot === null || link.receivedAtMono === null) return null;
  const age = Math.max(0, link.ageOnArrivalMs ?? 0);
  return link.snapshot.generated_at_unix_ms + age + Math.max(0, nowMono - link.receivedAtMono);
}

/// When the evidence for `s`'s green light expires, on the watchdog's clock:
/// the last verified success, or without one the recovery target's last
/// probe when it was answered, plus the snapshot's `freshness_secs`. A green
/// with neither has no evidence and has expired from the start
/// (`-Infinity`); any other light has nothing to expire (null), the grace
/// period's yellow after a wake included, which still reports the old
/// success. The watchdog itself drops a green once its evidence is stale,
/// but the window shows a snapshot until the next one, which can be minutes
/// later.
export function greenExpiresAtMs(s: TunnelStatus): number | null {
  if (s.light !== "green") return null;
  const last = s.recovery_target?.last_probe ?? null;
  const answered =
    last !== null && (last.result.result === "reply" || last.result.result === "refused")
      ? last.finished_at_unix_ms
      : null;
  const at = s.last_verified_unix_ms ?? answered;
  return at === null ? Number.NEGATIVE_INFINITY : at + s.freshness_secs * 1000;
}

/// Whether `s` has a green light whose evidence has expired at `nowMs` (the
/// watchdog's clock, `watchdogNowMs`).
export function greenExpired(s: TunnelStatus, nowMs: number): boolean {
  const at = greenExpiresAtMs(s);
  return at !== null && nowMs > at;
}

/// The tunnels whose green light has expired now (`greenExpired`), in
/// snapshot order; none while the watchdog is lost, whose overlay already
/// says no light is current.
export function expiredTunnels(link: Link, nowMono: number): TunnelId[] {
  const now = watchdogNowMs(link, nowMono);
  if (now === null || isLost(link, nowMono)) return [];
  return (link.snapshot?.tunnels ?? []).filter((s) => greenExpired(s, now)).map((s) => s.tunnel_id);
}

/// How long before a green's evidence expires the window asks for a new
/// snapshot, so with a watchdog that keeps probing the green goes on
/// without a gap (and the tray's dot without a blink).
export const EXPIRY_LEAD_MS = 2_000;
/// The soonest after a snapshot arrived that it is asked for again.
export const EXPIRY_MIN_AFTER_MS = 1_000;

/// When to ask for a snapshot because a green light's evidence is about to
/// expire: `EXPIRY_LEAD_MS` before the first of its greens expires, never
/// sooner than `EXPIRY_MIN_AFTER_MS` after it arrived, and once per
/// snapshot. A green already expired when its snapshot arrived does not
/// count, so a snapshot that is old on arrival never sets off a request
/// after request; the heartbeat brings the next one. The expired notice
/// itself is shown only once a green has expired (`greenExpired`).
export class ExpiryWatch {
  private snapshot: Snapshot | null = null;
  private arrivedMono = 0;
  private onArrival: ReadonlySet<TunnelId> = new Set();
  private asked = false;

  /// A snapshot was accepted at `nowMono`; `expired` are its tunnels
  /// expired already.
  arrived(snapshot: Snapshot, nowMono: number, expired: readonly TunnelId[]): void {
    this.snapshot = snapshot;
    this.arrivedMono = nowMono;
    this.onArrival = new Set(expired);
    this.asked = false;
  }

  /// Whether to ask now for a snapshot to replace `link`'s. True at most
  /// once for each snapshot told of with `arrived`, and never while the
  /// watchdog is lost.
  shouldRefresh(link: Link, nowMono: number): boolean {
    const snapshot = link.snapshot;
    if (snapshot === null || snapshot !== this.snapshot || this.asked || isLost(link, nowMono)) return false;
    if (nowMono - this.arrivedMono < EXPIRY_MIN_AFTER_MS) return false;
    const now = watchdogNowMs(link, nowMono);
    if (now === null) return false;
    const due = snapshot.tunnels.some((s) => {
      const at = greenExpiresAtMs(s);
      return at !== null && !this.onArrival.has(s.tunnel_id) && now >= at - EXPIRY_LEAD_MS;
    });
    if (due) this.asked = true;
    return due;
  }
}

export function targetLabel(target: ProbeTarget): string {
  return target.probe.kind === "icmp"
    ? t("probe.target_icmp", { address: target.address })
    : t("probe.target_tcp", { address: target.address, port: target.probe.port });
}

export function resultText(result: ProbeResult): string {
  switch (result.result) {
    case "reply":
      return t("probe.reply", { ms: result.rtt_ms });
    case "refused":
      return t("probe.refused", { ms: result.rtt_ms });
    case "no_reply":
      // A failure is never shown as 0 ms (plan §5.1).
      return t("probe.no_reply");
    case "error":
      return t("probe.error", { error: ADAPTER_ERROR[result.error], code: result.error });
  }
}

function failing(result: ProbeResult | undefined): boolean {
  return result !== undefined && (result.result === "no_reply" || result.result === "error");
}

/// Whether a result finished at most `freshSecs` (the tunnel's
/// `freshness_secs`) before the snapshot was made. The watchdog keeps an
/// observe target's last result while it does not probe it (paused,
/// monitoring off, a tunnel that is down, being stopped, started or
/// verified, or one it cannot read), so an old result is shown as old and
/// never counted as a host problem now. A result stamped after the snapshot
/// means the watchdog's clock moved back in between; it is not judged
/// current either.
export function isCurrent(finishedAtMs: number | null, generatedAtMs: number, freshSecs: number): boolean {
  if (finishedAtMs === null) return false;
  const age = generatedAtMs - finishedAtMs;
  return age >= 0 && age <= freshSecs * 1000;
}

function targetView(status: TargetStatus, generatedAtMs: number, freshSecs: number, lost: boolean): TargetView {
  const result = status.last_probe?.result;
  const atMs = status.last_probe?.finished_at_unix_ms ?? null;
  const current = !lost && isCurrent(atMs, generatedAtMs, freshSecs);
  return {
    label: targetLabel(status.target),
    result: result ? resultText(result) : t("probe.never"),
    failing: failing(result),
    current,
    atMs,
    note:
      current || atMs === null
        ? ""
        : lost
          ? t("probe.last_result")
          : t("probe.last_result_old", { secs: freshSecs }),
  };
}

/// Observe targets of `t` that are down now: failing, and current.
function hostIssuesOf(t: TunnelStatus, generatedAtMs: number): number {
  return t.observe_targets.filter(
    (o) =>
      failing(o.last_probe?.result) &&
      isCurrent(o.last_probe?.finished_at_unix_ms ?? null, generatedAtMs, t.freshness_secs),
  ).length;
}

/// The observe targets in one line, without its label.
function observeShort(targets: readonly TargetView[], issues: number): string {
  if (targets.length === 0) return t("observe.none");
  const old = targets.filter((o) => !o.current).length;
  if (old === targets.length) return t("observe.all_old", { count: targets.length });
  return old > 0
    ? t("observe.some_old", { count: targets.length, issues, old })
    : t("observe.current", { count: targets.length, issues });
}

const enabled: Availability = { kind: "enabled" };
const disabled = (why: string): Availability => ({ kind: "disabled", why });
/// Unavailable because the watchdog is not heard from; worded when used, in
/// the language of the moment.
const lostNow = (): Availability => disabled(t("common.lost"));

function controls(s: TunnelStatus, paused: boolean, lost: boolean) {
  if (lost) {
    const why = lostNow();
    return { connect: why, disconnect: why, probeNow: why, reconnect: why };
  }
  if (s.takeover === "binding_lost") {
    const why = disabled(t("card.why_binding_lost"));
    return { connect: why, disconnect: why, probeNow: why, reconnect: why };
  }
  const busy = s.recovery === "queued" || s.recovery === "stopping" || s.recovery === "starting";
  return {
    connect:
      s.intent.desired === "connected" && s.os_state === "connected"
        ? disabled(t("card.why_connected"))
        : enabled,
    disconnect:
      s.intent.desired === "disconnected" && s.os_state === "disconnected"
        ? disabled(t("card.why_disconnected"))
        : enabled,
    probeNow: paused
      ? disabled(t("card.why_paused"))
      : !s.intent.monitoring_enabled
        ? disabled(t("card.why_tunnel_paused"))
        : enabled,
    reconnect: busy ? disabled(t("card.why_busy")) : enabled,
  };
}

/// The settings form's starting values: the targets as typed ("address",
/// "address:port") and the timing.
function settingsOf(t: TunnelStatus, retrySupported: boolean): SettingsView {
  const recovery = t.recovery_target?.target;
  return {
    recoveryAddress: recovery ? recovery.address : "",
    recoveryPort: recovery && recovery.probe.kind === "tcp_connect" ? String(recovery.probe.port) : "",
    observeText: t.observe_targets
      .map((o) => (o.target.probe.kind === "icmp" ? o.target.address : `${o.target.address}:${o.target.probe.port}`))
      .join("\n"),
    probe: t.probe,
    retry: t.retry,
    retrySupported,
  };
}

/// How far the incident's automatic recovery has got against the tunnel's
/// retry limit, in words.
export function retriesText(s: TunnelStatus): string | null {
  const incident = s.incident;
  if (incident === null) return null;
  const used = incident.retries_used;
  if (s.retry.limit === null) return t("details.retries_forever", { used });
  if (incident.retries_exhausted) return t("details.retries_exhausted", { used, limit: s.retry.limit });
  return t("details.retries_limited", { used, limit: s.retry.limit });
}

/// A value in words followed by its code (`common.with_code`).
function withCode(text: string, code: string): string {
  return t("common.with_code", { text, code });
}

function row(
  s: TunnelStatus,
  paused: boolean,
  lost: boolean,
  label: string,
  updatedMs: number,
  notificationsOn: boolean,
  nowMs: number,
  retrySupported: boolean,
): RowView {
  const recovery = s.recovery_target;
  const last = recovery?.last_probe?.result;
  // A green whose evidence is too old is not shown as green, nor as any
  // other light: the watchdog has not said yet what it is now.
  const expired = !lost && greenExpired(s, nowMs);
  const light = lost || expired ? "unknown" : s.light;
  const LOST = lostNow();
  // Nothing is read while monitoring is paused, so the OS state is the last
  // known one, not the current one.
  const notRead = paused || !s.intent.monitoring_enabled;
  // Turning auto recovery off is always allowed while the watchdog answers;
  // turning it on needs a recovery target that passed its baseline check.
  const autoWhy = s.recovery_target === null ? t("card.why_no_target") : t("card.why_auto_blocked");
  const autoBlocked = !s.intent.auto_recovery_enabled && !s.auto_recovery_allowed;
  const autoToggle: Availability = lost ? LOST : autoBlocked ? disabled(autoWhy) : enabled;
  const details: Array<[string, string]> = [
    [t("details.tunnel_id"), s.tunnel_id],
    // The card shows the OS state in words; the details add its code.
    [t("details.os_state_code"), notRead ? t("details.not_updated_paused", { code: s.os_state }) : s.os_state],
    [t("details.data_health"), withCode(DATA_HEALTH[s.data_health], s.data_health)],
    [t("details.recovery"), withCode(RECOVERY_PHASE[s.recovery], s.recovery)],
    [t("details.takeover"), withCode(TAKEOVER[s.takeover], s.takeover)],
    [t("details.light_reason_code"), s.light_reason],
  ];
  if (s.fault) {
    // Its own text is in the fault's details on the card.
    details.push([t("details.fault"), withCode(ADAPTER_ERROR[s.fault.kind], s.fault.kind)]);
  }
  if (s.incident) {
    details.push(
      [t("details.incident"), s.incident.incident_id],
      [t("details.first_recovery"), FIRST_RECOVERY[s.incident.first_recovery]],
      [t("details.notification"), NOTIFICATION_STATE[s.incident.notification]],
    );
    const retries = retrySupported ? retriesText(s) : null;
    if (retries !== null) details.push([t("details.retries"), retries]);
  }
  const observe = s.observe_targets.map((o) => targetView(o, updatedMs, s.freshness_secs, lost));
  const issues = lost ? 0 : hostIssuesOf(s, updatedMs);
  const short = observeShort(observe, issues);
  return {
    id: s.tunnel_id,
    name: s.display_name,
    label,
    updatedMs,
    light,
    icon: lost ? "?" : expired ? EXPIRED_ICON : LIGHT[s.light].icon,
    lightLabel: lost ? t("common.unknown_state") : expired ? t("card.expired_label") : LIGHT[s.light].label,
    reason: lost
      ? t("card.reason_lost")
      : expired
        ? t("card.expired_reason", { secs: s.freshness_secs })
        : paused && s.light_reason === "monitoring_off"
          ? t("card.reason_paused_all")
          : LIGHT_REASON[s.light_reason],
    expired: expired ? t("card.expired") : null,
    fault: lost || s.fault === null ? null : faultSaid(s.fault),
    intended: !lost && s.light_reason === "disconnected_by_user",
    sinceMs: s.light_since_unix_ms,
    deadlineMs: lost ? null : s.deadline_unix_ms,
    nextRetryMs: lost ? null : s.next_retry_unix_ms,
    // A streak is counted only while the tunnel is probed.
    failures: lost || notRead ? null : failureProgress(s.consecutive_failures, s.probe),
    target: recovery ? targetLabel(recovery.target) : t("card.target_unset"),
    latency: last ? resultText(last) : "—",
    lastVerifiedMs: s.last_verified_unix_ms,
    osState: notRead ? t("card.os_before_pause", { state: OS_STATE[s.os_state] }) : OS_STATE[s.os_state],
    desired: s.intent.desired,
    monitoring: { on: s.intent.monitoring_enabled, toggle: lost ? LOST : enabled, note: "" },
    autoRecovery: {
      on: s.intent.auto_recovery_enabled,
      toggle: autoToggle,
      // An import asked for it: it turns on once the baseline passes.
      note: lost ? "" : s.auto_recovery_pending ? t("card.auto_pending") : autoBlocked ? autoWhy : "",
    },
    notifications: {
      on: s.notifications_enabled,
      toggle: lost ? LOST : enabled,
      note: notificationsOn ? "" : t("card.notify_off_note"),
    },
    startWhenStoppedExternally: {
      on: s.intent.start_when_stopped_externally,
      toggle: lost ? LOST : enabled,
      note: "",
    },
    ...controls(s, paused, lost),
    edit: lost ? LOST : enabled,
    unregister: lost ? LOST : enabled,
    settings: settingsOf(s, retrySupported),
    timing: timingSummary(s.probe),
    judgment: judgmentText(s.probe),
    judgmentShort: judgmentShort(s.probe),
    freshnessSecs: s.freshness_secs,
    details,
    observe,
    hostIssues: issues,
    observeSummary: t("observe.labelled", { summary: short }),
    observeShort: short,
  };
}

/// What each row is called: its display name, plus its tunnel ID when the
/// manager reports the same name for another tunnel too.
function labels(tunnels: readonly TunnelStatus[]): string[] {
  const seen = new Map<string, number>();
  for (const s of tunnels) seen.set(s.display_name, (seen.get(s.display_name) ?? 0) + 1);
  return tunnels.map((s) =>
    (seen.get(s.display_name) ?? 0) > 1
      ? t("common.name_with_id", { name: s.display_name, id: s.tunnel_id })
      : s.display_name,
  );
}

/// The tray dot, among the tunnels being monitored (monitoring on, and the
/// host not paused), for the owner's "every monitored tunnel is through":
/// red if any has a red light; green if none is red or yellow and at least
/// one is green (gray ones, such as a tunnel the user disconnected on
/// purpose, are left out, but gray alone is not green); none otherwise:
/// nothing monitored or only gray, a tunnel still starting or confirming,
/// the host paused, or the watchdog not heard from. A green whose evidence
/// has expired by `nowMs` (the watchdog's clock, `watchdogNowMs`) is not
/// known to be through, so it counts like a yellow one. The watchdog's
/// lights are only counted here; the GUI decides none.
export function trayDot(snapshot: Snapshot | null, lost: boolean, nowMs: number): TrayDot {
  if (lost || snapshot === null || snapshot.monitoring_paused) return "none";
  const monitored = snapshot.tunnels.filter((s) => s.intent.monitoring_enabled);
  if (monitored.some((s) => s.light === "red")) return "red";
  if (monitored.some((s) => s.light === "yellow" || greenExpired(s, nowMs))) return "none";
  return monitored.some((s) => s.light === "green") ? "green" : "none";
}

/// The most a tray summary promises its green dot, in milliseconds: the
/// longest freshness a tunnel can have (3,672 s: a 3,600 s interval, a 10 s
/// timeout, 18 s, and four observe targets at 11 s each), with a margin.
/// The shell refuses more (`GreenForMs` in `src-tauri/src/lib.rs`).
export const TRAY_GREEN_MAX_MS = 3_700_000;

/// How long from `nowMs` (the watchdog's clock, `watchdogNowMs`) the tray's
/// green dot `dot` may stay: until the first of the monitored greens that
/// make it green runs out of evidence (`greenExpiresAtMs`), in whole
/// milliseconds and at most `TRAY_GREEN_MAX_MS`; 0 when the dot is not
/// green.
export function trayGreenForMs(snapshot: Snapshot | null, dot: TrayDot, nowMs: number): number {
  if (dot !== "green" || snapshot === null) return 0;
  const until = Math.min(
    ...snapshot.tunnels
      .filter((s) => s.intent.monitoring_enabled)
      .map(greenExpiresAtMs)
      .filter((at): at is number => at !== null),
  );
  // A green dot always has a green behind it; without one nothing is
  // promised.
  if (!Number.isFinite(until)) return 0;
  return Math.max(0, Math.min(TRAY_GREEN_MAX_MS, Math.floor(until - nowMs)));
}

function monitorText(monitor: TraySummary["monitor"]): string {
  return t(`monitor.${monitor}`);
}

/// The tray's summary; `nowMs` is the watchdog's clock now
/// (`watchdogNowMs`), for the dot, its deadline and the expired greens.
export function tray(snapshot: Snapshot | null, lost: boolean, nowMs: number): TraySummary {
  const tunnels = snapshot?.tunnels ?? [];
  const paused = snapshot?.monitoring_paused ?? false;
  const vpnFaults = lost ? 0 : tunnels.filter((s) => s.light === "red").length;
  // Only a current result is a host problem now (see `isCurrent`).
  const generatedAt = snapshot?.generated_at_unix_ms ?? 0;
  const hostIssues = lost ? 0 : tunnels.reduce((n, s) => n + hostIssuesOf(s, generatedAt), 0);
  // The cards show such a green as expired (see `row`), never as fine.
  const expired = !lost && tunnels.some((s) => greenExpired(s, nowMs));
  const monitor: TraySummary["monitor"] = lost
    ? "lost"
    : snapshot?.storage === "failed"
      ? "storage_failed"
      : paused
        ? "paused"
        : "ok";
  const level: TraySummary["level"] = lost
    ? "lost"
    : vpnFaults > 0
      ? "fault"
      : expired
        ? "expired"
        : hostIssues > 0
          ? "host"
          : paused
            ? "paused"
            : "ok";
  const tooltip = lost
    ? t("summary.tooltip_lost", { monitor: monitorText(monitor) })
    : t("summary.tooltip", { vpn_faults: vpnFaults, host_issues: hostIssues, monitor: monitorText(monitor) });
  const dot = trayDot(snapshot, lost, nowMs);
  return { level, vpnFaults, hostIssues, monitor, dot, greenForMs: trayGreenForMs(snapshot, dot, nowMs), tooltip };
}

/// What `update_tray` is sent for `summary` (`TraySummary` in
/// `src-tauri/src/lib.rs`): fixed choices and numbers only, which the shell
/// words itself; the counts clamped to its 0–255; the window's refresh
/// interval in seconds, by which the shell judges the summary stale (no
/// news for max(15 s, 3 intervals), the window's own rule); and the green
/// dot's deadline.
export function trayCommand(summary: TraySummary, refreshSecs: number) {
  return {
    level: summary.level,
    vpn_faults: Math.min(summary.vpnFaults, 255),
    host_issues: Math.min(summary.hostIssues, 255),
    monitor: summary.monitor,
    dot: summary.dot,
    refresh_secs: refreshSecs,
    green_for_ms: summary.greenForMs,
  };
}

/// The header's watchdog state, with the snapshot's time (`header.link`),
/// so a watchdog that stops answering shows its last one.
function linkText(monitor: TraySummary["monitor"], snapshot: Snapshot | null, zone: Zone | null): string {
  if (snapshot === null) return t("header.lost", { icon: "■" });
  const at = formatShort(snapshot.generated_at_unix_ms, snapshot.generated_at_unix_ms, zone);
  if (monitor === "lost") {
    return t("header.lost_since", { icon: "■", at: formatDateTime(snapshot.generated_at_unix_ms, zone) });
  }
  const icon = monitor === "ok" ? "●" : monitor === "paused" ? "○" : "▲";
  return t("header.link", { icon, monitor: monitorText(monitor), at });
}

/// The view at `nowMono` (`performance.now()`); the snapshot's age when it
/// arrived is in `link` (see `watchdogNowMs`).
export function viewModel(link: Link, nowMono: number): ViewModel {
  const lost = isLost(link, nowMono);
  const snapshot = link.snapshot;
  const paused = snapshot?.monitoring_paused ?? false;
  const zone = zoneOf(snapshot?.host.time_zone);
  const error = lost && link.lastError !== null ? link.lastError() : null;
  const overlay: Overlay | null = lost
    ? {
        title: t("common.lost"),
        text:
          error !== null
            ? t("overlay.error", { error: error.text })
            : t("overlay.silent", { secs: Math.round((link.lostAfterMs ?? LOST_AFTER_MS) / 1000) }),
        detail: error?.detail ?? null,
      }
    : null;
  const banner =
    !lost && snapshot?.storage === "failed"
      ? t("banner.storage_failed")
      : !lost && paused
        ? t("banner.paused")
        : null;
  const tunnels = snapshot?.tunnels ?? [];
  const names = labels(tunnels);
  const updated = snapshot?.generated_at_unix_ms ?? 0;
  const notificationsOn = snapshot?.notification.enabled ?? false;
  const now = watchdogNowMs(link, nowMono) ?? updated;
  // Nothing new is offered while the watchdog that would answer is unknown.
  const retrySupported = offeredSince(lost ? null : (snapshot?.host.watchdog_version ?? null), RECOVERY_RETRY_SINCE);
  const rows = tunnels.map((s, i) =>
    row(s, paused, lost, names[i] ?? s.display_name, updated, notificationsOn, now, retrySupported),
  );
  const summary = tray(snapshot, lost, now);
  const summaryParts = summaryLine(snapshot, rows, summary);
  return {
    overlay,
    paused,
    banner,
    rows,
    tray: summary,
    summary: summaryParts.map((p) => p.text).join(""),
    summaryParts,
    link: { monitor: summary.monitor, text: linkText(summary.monitor, snapshot, zone) },
    zone,
  };
}

/// What the header says about a storage failure, in the banner's words: it
/// stays in view when the banner has scrolled away with a long list.
export function storageFailedClause(): string {
  return t("summary.storage_failed");
}

/// A piece of the header's summary line. `tone` colours it as the card of
/// that light is coloured (`red` also for what is wrong outside a light: a
/// host target's fault, a storage failure); null leaves it the header's
/// own colour.
export interface SummaryPart {
  readonly text: string;
  readonly tone: Light | "expired" | null;
}

/// The whole list in one line (`summary.*`), in parts: the count of each
/// light, worst first and only the lights present, each in its light's
/// colour, with expired greens counted apart after the yellow ones, so a
/// long list can be judged without scrolling it, then whatever keeps the
/// watchdog from acting on its own (a storage failure, the pause). Its
/// length does not grow with the number of tunnels.
function summaryLine(snapshot: Snapshot | null, rows: readonly RowView[], tray: TraySummary): SummaryPart[] {
  const plainPart = (text: string): SummaryPart => ({ text, tone: null });
  if (tray.monitor === "lost") {
    return [plainPart(snapshot !== null ? t("summary.lost", { count: rows.length }) : t("summary.lost_none"))];
  }
  const count = (tone: SummaryPart["tone"], icon: string, label: string, n: number): SummaryPart => ({
    text: t("summary.light_count", { icon, label, count: n }),
    tone,
  });
  const lights = (order: readonly Light[]) =>
    order
      .map((light) => [light, rows.filter((r) => r.light === light).length] as const)
      .filter(([, n]) => n > 0)
      .map(([light, n]) => count(light, LIGHT[light].icon, LIGHT[light].label, n));
  const expired = rows.filter((r) => r.expired !== null).length;
  const counts = [
    ...lights(["red", "yellow"]),
    ...(expired > 0 ? [count("expired", EXPIRED_ICON, t("card.expired_label"), expired)] : []),
    ...lights(["green", "gray"]),
  ];
  const clauses: SummaryPart[][] = [];
  if (rows.length === 0) {
    clauses.push([plainPart(t("summary.none"))]);
  } else {
    // The counts go where `{lights}` stands in the sentence, which may put
    // words on either side of them; without it, after the sentence.
    const [before, after] = around("summary.counts", "lights", { count: rows.length }) ?? [
      t("summary.counts", { count: rows.length, lights: "" }),
      "",
    ];
    const separator = plainPart(t("summary.list_separator"));
    clauses.push([
      ...(before === "" ? [] : [plainPart(before)]),
      ...counts.flatMap((c, i) => (i === 0 ? [c] : [separator, c])),
      ...(after === "" ? [] : [plainPart(after)]),
    ]);
  }
  if (tray.hostIssues > 0) clauses.push([{ text: t("summary.host_issues", { count: tray.hostIssues }), tone: "red" }]);
  if (snapshot?.storage === "failed") clauses.push([{ text: storageFailedClause(), tone: "red" }]);
  if (snapshot?.monitoring_paused) clauses.push([plainPart(t("summary.paused"))]);
  return clauses.flatMap((clause, i) => (i === 0 ? clause : [plainPart(t("summary.clause_separator")), ...clause]));
}

/// The watchdog's answer to one registration from this window, kept until
/// the next discovery. Whether a tunnel is on the installation's access
/// list is known only from such an answer (plan §9.2): the GUI never guesses
/// it beforehand.
/// A refusal or a failure keeps the words for it, which are worked out in
/// the language of the moment.
export type RegisterAnswer =
  | { kind: "pending" }
  | { kind: "accepted" }
  | { kind: "refused"; words: Words }
  | { kind: "no_answer"; words: Words };

export interface DiscoveredView {
  id: TunnelId;
  /// The display name and the tunnel ID, which tell apart tunnels with the
  /// same name.
  label: string;
  unsupported: boolean;
  action: string;
  register: Availability;
  /// What happened to it, with the original text of a refusal for the
  /// details.
  status: Said;
}

export interface DiscoveryView {
  entries: DiscoveredView[];
  /// Said when the list may be cut at the contract's limit, or when a
  /// discovery found nothing: then the official WireGuard app may not be
  /// installed, or has no tunnel the watchdog can see (owner request of
  /// 2026-11-05).
  note: string | null;
}

/// Where the official WireGuard apps are, for the hint when discovery
/// finds nothing. Shown as text only: the window opens no links.
export const WIREGUARD_INSTALL_URL = "https://www.wireguard.com/install/";

export function discoveryView(
  discovered: readonly DiscoveredTunnel[],
  snapshot: Snapshot | null,
  answers: ReadonlyMap<TunnelId, RegisterAnswer>,
  lost: boolean,
  searched = false,
): DiscoveryView {
  const registered = new Map((snapshot?.tunnels ?? []).map((s) => [s.tunnel_id, s] as const));
  const full = registered.size >= MAX_TUNNELS;
  const entries = discovered.map((d): DiscoveredView => {
    const known = registered.get(d.tunnel_id);
    // The watchdog takes a lost binding back when the tunnel is registered
    // again, even with the list full.
    const relink = known?.takeover === "binding_lost";
    const answer = answers.get(d.tunnel_id);
    const register: Availability = lost
      ? lostNow()
      : d.control === "unsupported"
        ? disabled(t("discovered.why_unsupported"))
        : known && !relink
          ? disabled(t("discovered.registered"))
          : !known && full
            ? disabled(t("discovered.why_full", { max: MAX_TUNNELS }))
            : answer?.kind === "pending"
              ? disabled(t("discovered.why_pending"))
              : enabled;
    const refused = answer?.kind === "refused" ? answer.words() : null;
    const status: Said =
      known && !relink
        ? plain(t("discovered.registered"))
        : answer?.kind === "pending"
          ? plain(t("discovered.pending"))
          : answer?.kind === "accepted"
            ? plain(t("discovered.accepted"))
            : refused !== null
              ? { text: t("discovered.refused", { text: refused.text }), detail: refused.detail }
              : answer?.kind === "no_answer"
                ? answer.words()
                : plain(relink ? t("discovered.relink_note") : "");
    return {
      id: d.tunnel_id,
      label: t("common.name_with_id", { name: d.display_name, id: d.tunnel_id }),
      unsupported: d.control === "unsupported",
      action: relink ? t("discovered.relink") : t("discovered.register"),
      register,
      status,
    };
  });
  return {
    entries,
    note:
      discovered.length >= MAX_DISCOVERED
        ? t("discovered.capped", { max: MAX_DISCOVERED })
        : searched && discovered.length === 0
          ? t("discovered.none_hint", { url: WIREGUARD_INSTALL_URL })
          : null,
  };
}

/// The time left until `targetMs` (`countdown.*`), measured against the
/// snapshot's own clock (the watchdog's wall time at `generatedAtMs`, plus
/// monotonic time elapsed since it arrived), so a local wall clock change
/// cannot distort it. At or past zero it says to wait for the next update,
/// never guessing the result.
export function countdown(targetMs: number, generatedAtMs: number, elapsedMono: number): string {
  const left = Math.ceil((targetMs - (generatedAtMs + elapsedMono)) / 1000);
  if (left <= 0) {
    return t("countdown.waiting");
  }
  const minutes = Math.floor(left / 60);
  const seconds = left % 60;
  return minutes > 0
    ? t("countdown.minutes", { minutes, seconds: String(seconds).padStart(2, "0") })
    : t("countdown.seconds", { seconds });
}

export interface NotificationView {
  enabled: boolean;
  deviceName: string;
  /// The sentence that says which host name messages give while no device
  /// name is set (blank, as the watchdog judges it: nothing but spaces
  /// counts), shown beside the field and as its placeholder; "" when a
  /// device name is set or the watchdog reports no host name (watchdog
  /// 4.1.0, owner decision of 2026-11-19).
  deviceHost: string;
  chatId: string;
  token: string;
  test: string | null;
  testBusy: boolean;
  lastFailure: string | null;
}

/// The name messages give this computer, as the watchdog picks it: the
/// device name unless it is blank, else the host name, else "" (and the
/// language's unnamed-device text is written). The preview uses it.
export function messageDeviceName(n: Snapshot["notification"] | undefined): string {
  if (!n) return "";
  return blankName(n.device_name) ? (n.host_name ?? "") : n.device_name;
}

export function notificationView(snapshot: Snapshot | null): NotificationView {
  const n = snapshot?.notification;
  const test = n?.last_test ?? null;
  return {
    enabled: n?.enabled ?? false,
    deviceName: n?.device_name ?? "",
    deviceHost:
      n?.host_name && blankName(n.device_name) ? t("notify.device_host", { name: n.host_name }) : "",
    chatId: n?.chat_id ?? "",
    // Without a snapshot nothing is known, not even "not set".
    token: n ? CREDENTIAL[n.token] : t("common.unknown_state"),
    test:
      test === null
        ? null
        : t(test.failure ? "notify.test_result_failure" : "notify.test_result", {
            state: TEST_STATE[test.state],
            failure: test.failure ? NOTIFICATION_FAILURE[test.failure] : "",
            time: formatDateTime(
              test.finished_at_unix_ms ?? test.requested_at_unix_ms,
              zoneOf(snapshot?.host.time_zone),
            ),
          }),
    testBusy: test?.state === "sending",
    lastFailure: n?.last_failure ? NOTIFICATION_FAILURE[n.last_failure] : null,
  };
}
