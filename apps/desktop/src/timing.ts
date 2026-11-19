// A tunnel's probe timing in words: how many failed rounds a fault needs and
// how long after traffic stops it is confirmed. The arithmetic mirrors
// `wgal_protocol::ProbeTiming` (`failures_needed`, `rounds_to_confirm`) and
// the freshness the watchdog reports, and is tested with the same worked
// examples; the watchdog alone confirms faults and judges freshness, so this
// only explains its settings. What a tunnel's results count for is always
// the snapshot's `freshness_secs`, never `freshnessSecs` here.

import { t } from "./i18n";
import type { ProbeTiming } from "./protocol";

/// Slack the contract adds to one interval and one timeout before a probe
/// result goes stale (`FRESHNESS_MARGIN_SECS`).
const FRESHNESS_MARGIN_SECS = 18;

/// Consecutive failed rounds a fault needs: `retries + 1`.
export function failuresNeeded(t: ProbeTiming): number {
  return t.retries + 1;
}

/// Rounds after the streak's first failed round at which the fault is
/// confirmed: max(retries, ceil(min_fault_secs / interval_secs)).
export function roundsToConfirm(t: ProbeTiming): number {
  return Math.max(t.retries, Math.ceil(t.min_fault_secs / t.interval_secs));
}

/// How old a probe result may be and still count, in seconds, as the
/// watchdog works it out for a tunnel with `observeTargets` observe targets
/// (`TunnelStatus.freshness_secs`): one interval, one timeout and the margin,
/// plus a timeout and a second for each observe target probed in the same
/// round. For the dev fake and previews only.
export function freshnessSecs(t: ProbeTiming, observeTargets: number): number {
  return t.interval_secs + t.timeout_secs + FRESHNESS_MARGIN_SECS + observeTargets * (t.timeout_secs + 1);
}

/// Seconds from traffic stopping to the fault being confirmed: at least
/// k × interval + timeout, at most (k + 1) × interval + timeout, depending
/// on where in a round the traffic stopped.
export function judgmentWindow(t: ProbeTiming): { fromSecs: number; toSecs: number } {
  const k = roundsToConfirm(t);
  return {
    fromSecs: k * t.interval_secs + t.timeout_secs,
    toSecs: (k + 1) * t.interval_secs + t.timeout_secs,
  };
}

/// A whole number of seconds in hours and minutes, minutes and seconds, or
/// seconds (`duration.*`), leaving out a zero second part.
export function durationText(secs: number): string {
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  if (h > 0) return m > 0 ? t("duration.hours_minutes", { hours: h, minutes: m }) : t("duration.hours", { hours: h });
  if (m > 0) return s > 0 ? t("duration.minutes_seconds", { minutes: m, seconds: s }) : t("duration.minutes", { minutes: m });
  return t("duration.seconds", { seconds: s });
}

/// The judgment time the owner asked to see next to the settings
/// (`timing.judgment`): from when to when after traffic stops a fault is
/// confirmed, and what it needs. Long windows also get minutes or hours.
export function judgmentText(p: ProbeTiming): string {
  const { fromSecs, toSecs } = judgmentWindow(p);
  const about = toSecs >= 120 ? t("timing.about", { from: durationText(fromSecs), to: durationText(toSecs) }) : "";
  const atLeast = p.min_fault_secs > 0 ? t("timing.at_least", { secs: p.min_fault_secs }) : "";
  return t("timing.judgment", { from: fromSecs, to: toSecs, about, needed: failuresNeeded(p), at_least: atLeast });
}

/// The short form for a collapsed card (`timing.short`).
export function judgmentShort(p: ProbeTiming): string {
  const { fromSecs, toSecs } = judgmentWindow(p);
  const window =
    toSecs >= 120
      ? t("timing.window_long", { from: durationText(fromSecs), to: durationText(toSecs) })
      : t("timing.window_secs", { from: fromSecs, to: toSecs });
  return t("timing.short", { interval: durationText(p.interval_secs), window });
}

/// The retry progress of a failing tunnel (`timing.failures`), or null when
/// nothing has failed. Past the count needed it says so instead of showing
/// a fraction above one.
export function failureProgress(consecutive: number, p: ProbeTiming): string | null {
  if (consecutive <= 0) return null;
  const needed = failuresNeeded(p);
  return consecutive <= needed
    ? t("timing.failures", { count: consecutive, needed })
    : t("timing.failures_past", { count: consecutive, needed });
}

/// The whole timing in one line (`timing.summary`).
export function timingSummary(p: ProbeTiming): string {
  return t("timing.summary", {
    interval: durationText(p.interval_secs),
    timeout: p.timeout_secs,
    retries: p.retries,
    min_fault: durationText(p.min_fault_secs),
  });
}
