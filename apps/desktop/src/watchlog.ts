// The log tab's watchdog-link view (`log.filter_watchdog`, owner's request
// of 2026-11-03), without the DOM. The watchdog logs its own starts and
// stops; while it is gone it cannot log anything, so this window keeps a
// short journal of what it saw itself: that it could not reach the watchdog
// (judged by the threshold and the failed answers of the lost-link
// overlay), that it
// reached it again and after how long, and a start that never reached it.
// The journal is kept per viewer in localStorage, at most 72 hours and 500
// records, and written only when the link changes, never on a refresh. It
// holds kinds and times only, never an error's text. The view merges it
// with the watchdog's own lines, newest first.

import { REFERENCE as R, t, table } from "./i18n";
import { CATEGORY, type LogEntryView, entryView } from "./log";
import type { KeyValueStore } from "./prefs";
import type { EventEntry, ProtocolError, TunnelId } from "./protocol";
import { type Zone, formatCompact } from "./time";

/// Why the window could not reach the watchdog, as far as the answer tells:
/// the service is not running (no pipe), the watchdog refused this account,
/// the pipe is not the WGAL service's, no answer in time, or anything else.
export type LinkError = "not_running" | "refused" | "not_the_watchdog" | "timeout" | "other";

export const LINK_ERROR = table("link_error", R.link_error satisfies Record<LinkError, string>);

/// The kind of a failed snapshot answer, by its reason (the shell's own
/// refusals carry one: `not_running`, `not_the_watchdog`,
/// `caller_not_allowed`, `no_answer_in_time`…), else by its code; `null`
/// when the request threw instead of being answered.
export function linkError(error: ProtocolError | null): LinkError {
  if (error === null) return "other";
  switch (error.reason) {
    case "not_running":
      return "not_running";
    case "not_the_watchdog":
      return "not_the_watchdog";
    case "caller_not_allowed":
    case "needs_administrator_or_listed_tunnel":
      return "refused";
    case "no_answer_in_time":
    case "too_many_waiting":
      return "timeout";
    case null:
      return error.code === "unauthorized" ? "refused" : error.code === "timeout" ? "timeout" : "other";
    default:
      return "other";
  }
}

/// What the window saw. `lost`: it could reach the watchdog and then could
/// not (`lastOkMs` is the last good snapshot's time; `error` is null when
/// no answer came at all). `unreachable`: it started and could not reach it.
/// `restored`: it reached it again `goneMs` after noticing the loss, or, with
/// `goneMs` null, when it started again after being closed while the
/// watchdog was gone, so when it came back is not known.
export type LinkEvent =
  | { kind: "lost"; lastOkMs: number | null; error: LinkError | null }
  | { kind: "unreachable"; error: LinkError | null }
  | { kind: "restored"; goneMs: number | null };

export interface LinkRecord {
  id: number;
  /// When the window noticed it, on its own wall clock.
  atMs: number;
  event: LinkEvent;
}

/// The localStorage key, beside `wgal.tab` and `wgal.refreshSecs`.
export const JOURNAL_KEY = "wgal.watchdogLink";
export const JOURNAL_KEEP_MS = 72 * 3_600_000;
export const JOURNAL_MAX = 500;

const ERRORS = new Set<string>(["not_running", "refused", "not_the_watchdog", "timeout", "other"]);

const isTime = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v) && v >= 0;
const isError = (v: unknown): v is LinkError | null => v === null || (typeof v === "string" && ERRORS.has(v));

/// A stored record, or null when it is not one this version wrote.
function readRecord(value: unknown): LinkRecord | null {
  if (typeof value !== "object" || value === null) return null;
  const { id, atMs, event } = value as Record<string, unknown>;
  if (!Number.isSafeInteger(id) || (id as number) < 1 || !isTime(atMs)) return null;
  if (typeof event !== "object" || event === null) return null;
  const e = event as Record<string, unknown>;
  let parsed: LinkEvent;
  if (e.kind === "lost" && (e.lastOkMs === null || isTime(e.lastOkMs)) && isError(e.error)) {
    parsed = { kind: "lost", lastOkMs: e.lastOkMs, error: e.error };
  } else if (e.kind === "unreachable" && isError(e.error)) {
    parsed = { kind: "unreachable", error: e.error };
  } else if (e.kind === "restored" && (e.goneMs === null || isTime(e.goneMs))) {
    parsed = { kind: "restored", goneMs: e.goneMs };
  } else {
    return null;
  }
  return { id: id as number, atMs, event: parsed };
}

/// Records within 72 hours of `nowMs` (either way: a clock set far ahead
/// once does not keep its records), oldest first, the newest 500.
export function pruned(records: readonly LinkRecord[], nowMs: number): LinkRecord[] {
  return records
    .filter((r) => Math.abs(nowMs - r.atMs) <= JOURNAL_KEEP_MS)
    .sort((a, b) => a.atMs - b.atMs || a.id - b.id)
    .slice(-JOURNAL_MAX);
}

/// The window's journal. Storage is a convenience: every access may throw,
/// and without it the records live until the window closes and the tab still
/// works. Only `add` writes, so storage is touched when the link changes and
/// never on a refresh.
export class LinkJournal {
  private records: LinkRecord[] = [];
  /// The records are kept beyond this window.
  persistent: boolean;

  constructor(
    private readonly storage: KeyValueStore | null,
    nowMs: number,
  ) {
    const stored = this.read();
    this.persistent = stored !== null;
    this.records = pruned(stored ?? [], nowMs);
  }

  /// The stored records, or null when there is no storage or it throws. A
  /// stored value that is not a journal is an empty one.
  private read(): LinkRecord[] | null {
    if (this.storage === null) return null;
    try {
      const text = this.storage.getItem(JOURNAL_KEY);
      if (text === null) return [];
      const value = JSON.parse(text) as unknown;
      const list = (value as { records?: unknown } | null)?.records;
      if (!Array.isArray(list)) return [];
      return list.map(readRecord).filter((r): r is LinkRecord => r !== null);
    } catch (e) {
      // A garbled value is an empty journal; a storage that throws is none.
      return e instanceof SyntaxError ? [] : null;
    }
  }

  /// Adds what the window just saw and stores the journal, pruned. Another
  /// window may have added records since: they are kept.
  add(event: LinkEvent, nowMs: number): LinkRecord {
    const base = this.persistent ? (this.read() ?? this.records) : this.records;
    const id = base.reduce((max, r) => Math.max(max, r.id), 0) + 1;
    const record: LinkRecord = { id, atMs: nowMs, event };
    this.records = pruned([...base, record], nowMs);
    try {
      if (this.storage === null) throw new Error("no storage");
      this.storage.setItem(JOURNAL_KEY, JSON.stringify({ v: 1, records: this.records }));
      this.persistent = true;
    } catch {
      this.persistent = false;
    }
    return record;
  }

  /// Newest first.
  list(): LinkRecord[] {
    return [...this.records].reverse();
  }

  /// The record added last (the highest number), whatever the clock said.
  newest(): LinkRecord | undefined {
    return this.records.reduce<LinkRecord | undefined>((a, r) => (a === undefined || r.id > a.id ? r : a), undefined);
  }
}

/// A snapshot answer the window used: a snapshot made at `generatedAtMs`,
/// or a failure.
export type SnapshotAnswer = { ok: true; generatedAtMs: number } | { ok: false; error: LinkError };

/// Turns the window's snapshot requests and their answers into journal
/// records, one per change. A failed answer is a loss at once, as on the
/// overlay; so is a request left unanswered past the lost-link threshold.
/// That age is measured from when the request was sent, not from the last
/// snapshot, so a hidden window whose timers the system slows down (and so
/// asks less often) records nothing it did not see; and when the window's
/// own clock jumps (sleep, a suspended window), requests still out are
/// treated as sent at that moment.
export class LinkWatch {
  private phase: "starting" | "up" | "down" = "starting";
  private lastOkMs: number | null = null;
  private downAtMs: number | null = null;
  /// Snapshot requests sent and not yet answered: number → monotonic time.
  private readonly pending = new Map<number, number>();
  private lastCheckMono: number | null = null;

  constructor(
    private readonly journal: LinkJournal,
    private readonly lostAfterMs: () => number,
  ) {}

  /// Snapshot request `n` was sent.
  sent(n: number, nowMono: number): void {
    this.pending.set(n, nowMono);
  }

  /// The answer to request `n` was used (newer than any used before; older
  /// ones are dropped by the caller). Returns whether a record was added.
  answered(n: number, answer: SnapshotAnswer, nowMs: number): boolean {
    for (const k of this.pending.keys()) if (k <= n) this.pending.delete(k);
    if (!answer.ok) return this.down(answer.error, nowMs);
    this.lastOkMs = answer.generatedAtMs;
    const was = this.phase;
    this.phase = "up";
    if (was === "down") {
      this.journal.add({ kind: "restored", goneMs: Math.max(0, nowMs - (this.downAtMs ?? nowMs)) }, nowMs);
      return true;
    }
    // Closed while the watchdog was gone, and it is back now.
    const last = this.journal.newest();
    if (was === "starting" && last !== undefined && last.event.kind !== "restored") {
      this.journal.add({ kind: "restored", goneMs: null }, nowMs);
      return true;
    }
    return false;
  }

  /// Every second: a request out for longer than the threshold is a loss.
  /// Returns whether a record was added.
  check(nowMono: number, nowMs: number): boolean {
    const gap = this.lastCheckMono === null ? 0 : nowMono - this.lastCheckMono;
    this.lastCheckMono = nowMono;
    const limit = this.lostAfterMs();
    if (gap > limit) {
      // The window itself was not running; nothing was seen meanwhile.
      for (const k of this.pending.keys()) this.pending.set(k, nowMono);
      return false;
    }
    let oldest = Infinity;
    for (const sentAt of this.pending.values()) oldest = Math.min(oldest, sentAt);
    return nowMono - oldest > limit ? this.down(null, nowMs) : false;
  }

  private down(error: LinkError | null, nowMs: number): boolean {
    if (this.phase === "down") return false;
    const event: LinkEvent =
      this.phase === "starting" ? { kind: "unreachable", error } : { kind: "lost", lastOkMs: this.lastOkMs, error };
    this.phase = "down";
    this.downAtMs = nowMs;
    this.journal.add(event, nowMs);
    return true;
  }
}

/// How long a gap lasted, at most two units, the smaller one padded:
/// seconds, minutes and seconds, hours and minutes, or days and hours
/// (`duration.*`).
export function durationText(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  const pad = (n: number) => String(n).padStart(2, "0");
  if (s < 60) return t("duration.seconds", { seconds: s });
  if (s < 3_600) return t("duration.minutes_seconds", { minutes: Math.floor(s / 60), seconds: pad(s % 60) });
  if (s < 86_400) {
    return t("duration.hours_minutes", { hours: Math.floor(s / 3_600), minutes: pad(Math.floor((s % 3_600) / 60)) });
  }
  return t("duration.days_hours", { days: Math.floor(s / 86_400), hours: Math.floor((s % 86_400) / 3_600) });
}

/// The sentence for one record; `time` writes a time in the configured zone.
export function linkText(event: LinkEvent, time: (unixMs: number) => string): string {
  const why = (error: LinkError | null) => (error === null ? t("link.no_answer") : LINK_ERROR[error]);
  switch (event.kind) {
    case "lost":
      return event.lastOkMs === null
        ? t("link.lost", { why: why(event.error) })
        : t("link.lost_since", { why: why(event.error), at: time(event.lastOkMs) });
    case "unreachable":
      return t("link.unreachable", { why: why(event.error) });
    case "restored":
      return event.goneMs === null
        ? t("link.restored_unknown")
        : t("link.restored", { gone: durationText(event.goneMs) });
  }
}

/// A record's fields as `kind=link_lost error=not_running last_ok_unix_ms=…`.
export function linkFields(event: LinkEvent): string {
  switch (event.kind) {
    case "lost":
      return `kind=link_lost error=${event.error ?? "no_answer"} last_ok_unix_ms=${event.lastOkMs ?? "none"}`;
    case "unreachable":
      return `kind=link_unreachable_at_start error=${event.error ?? "no_answer"}`;
    case "restored":
      return `kind=link_restored gone_ms=${event.goneMs ?? "unknown"}`;
  }
}

/// Who a line comes from, in the log's tunnel column: the window's own
/// records, and in this view the watchdog's own lines (the host's under
/// every line).
export function guiLabel(): string {
  return t("log.who_gui");
}

export function watchdogLabel(): string {
  return t("log.who_watchdog");
}

/// Lost and restored have shapes of their own besides the color; both are
/// in the category `info`.
export const LINK_ICON = { down: "⚠", restored: "✓" } as const;
export const CATEGORY_OF_LINK = CATEGORY.info;

/// One line of the merged view: a watchdog line (`unclean`: a start with no
/// stop since the start before it) or one of the window's records.
export type WatchdogLine =
  | { key: string; source: "watchdog"; entry: EventEntry; unclean: boolean }
  | { key: string; source: "gui"; record: LinkRecord };

/// The watchdog's start and stop lines (newest first, as `ReadEventLog`
/// with the `watchdog` filter pages them) merged with the window's records
/// (newest first), newest first by time; at the same time the window's
/// record comes first, as it follows what the watchdog did. While older
/// watchdog lines are still to be paged in, records older than the oldest
/// line shown wait for them, so paging never leaves a gap; without any
/// watchdog line (none, or the log cannot be read) every record shows.
export function watchdogLines(
  entries: readonly EventEntry[],
  olderExist: boolean,
  records: readonly LinkRecord[],
): WatchdogLine[] {
  const oldest = entries[entries.length - 1];
  const cutoff = olderExist && oldest !== undefined ? oldest.at_unix_ms : -Infinity;
  const gui = records.filter((r) => r.atMs >= cutoff);
  // A start right after another start, among the lines shown: the oldest
  // start shown is not judged, as the line before it is not known yet.
  const lifecycle = entries.filter((e) => e.event.kind === "watchdog_started" || e.event.kind === "watchdog_stopped");
  const unclean = new Set<number>();
  lifecycle.forEach((e, k) => {
    if (e.event.kind === "watchdog_started" && lifecycle[k + 1]?.event.kind === "watchdog_started") unclean.add(e.seq);
  });
  const lines: WatchdogLine[] = [];
  let i = 0;
  let j = 0;
  while (i < entries.length || j < gui.length) {
    const entry = entries[i];
    const record = gui[j];
    if (record !== undefined && (entry === undefined || record.atMs >= entry.at_unix_ms)) {
      lines.push({ key: `g${record.id}-${record.atMs}`, source: "gui", record });
      j += 1;
    } else if (entry !== undefined) {
      lines.push({ key: `w${entry.seq}`, source: "watchdog", entry, unclean: unclean.has(entry.seq) });
      i += 1;
    }
  }
  return lines;
}

/// Said after a watchdog start that followed another start.
export function uncleanStop(): string {
  return t("log.unclean_stop");
}

/// How a log row looks; `tone` colors the window's own records, whose icon
/// and words already tell them apart.
export interface LogLineView extends Omit<LogEntryView, "seq"> {
  tone?: "down" | "restored";
}

/// A merged line as the log tab shows it: times in the configured zone,
/// without the year of `refMs` (the snapshot's time), as `entryView`.
export function watchdogLineView(
  line: WatchdogLine,
  names: ReadonlyMap<TunnelId, string>,
  zone: Zone | null,
  refMs: number,
): LogLineView {
  if (line.source === "watchdog") {
    const view = { ...entryView(line.entry, names, zone, refMs), who: watchdogLabel() };
    return line.unclean ? { ...view, text: t("log.with_note", { text: view.text, note: uncleanStop() }) } : view;
  }
  const time = (ms: number) => formatCompact(ms, refMs, zone);
  const { event, atMs } = line.record;
  const down = event.kind !== "restored";
  return {
    time: time(atMs),
    category: "info",
    icon: down ? LINK_ICON.down : LINK_ICON.restored,
    categoryLabel: CATEGORY_OF_LINK.label,
    who: guiLabel(),
    text: linkText(event, time),
    tone: down ? "down" : "restored",
  };
}
