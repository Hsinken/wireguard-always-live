// Times shown in the window are written in the zone the watchdog writes its
// log and Telegram messages in (`snapshot.host.time_zone`): the effective
// IANA zone through `Intl.DateTimeFormat`, or, when the system zone has no
// IANA name, the reported offset from UTC. Before any snapshot the window's
// own zone is used.

import { t } from "./i18n";
import type { TimeZoneStatus } from "./protocol";

export interface Zone {
  /// The IANA name, or null to use `offsetSecs`.
  readonly name: string | null;
  readonly offsetSecs: number;
}

export function zoneOf(status: TimeZoneStatus | null | undefined): Zone | null {
  return status ? { name: status.effective, offsetSecs: status.utc_offset_secs } : null;
}

interface Parts {
  year: number;
  month: number;
  day: number;
  hour: number;
  minute: number;
  second: number;
}

/// One formatter per zone name: building one is slow, and a render formats
/// up to a hundred log times. A name this engine does not know maps to null
/// and falls back to the offset.
const formatters = new Map<string, Intl.DateTimeFormat | null>();

function formatter(name: string): Intl.DateTimeFormat | null {
  let f = formatters.get(name);
  if (f === undefined) {
    try {
      f = new Intl.DateTimeFormat("en-US", {
        timeZone: name,
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
        hourCycle: "h23",
      });
    } catch {
      f = null;
    }
    formatters.set(name, f);
  }
  return f;
}

function partsIn(unixMs: number, zone: Zone | null): Parts {
  if (zone === null) {
    const d = new Date(unixMs);
    return {
      year: d.getFullYear(),
      month: d.getMonth() + 1,
      day: d.getDate(),
      hour: d.getHours(),
      minute: d.getMinutes(),
      second: d.getSeconds(),
    };
  }
  const f = zone.name === null ? null : formatter(zone.name);
  if (f !== null) {
    const parts = f.formatToParts(unixMs);
    const value = (type: Intl.DateTimeFormatPartTypes) =>
      Number(parts.find((p) => p.type === type)?.value ?? "0");
    return {
      year: value("year"),
      month: value("month"),
      day: value("day"),
      hour: value("hour"),
      minute: value("minute"),
      second: value("second"),
    };
  }
  const d = new Date(unixMs + zone.offsetSecs * 1000);
  return {
    year: d.getUTCFullYear(),
    month: d.getUTCMonth() + 1,
    day: d.getUTCDate(),
    hour: d.getUTCHours(),
    minute: d.getUTCMinutes(),
    second: d.getUTCSeconds(),
  };
}

const pad = (n: number) => String(n).padStart(2, "0");

function datePart(p: Parts): string {
  return `${p.year}-${pad(p.month)}-${pad(p.day)}`;
}

function clockPart(p: Parts): string {
  return `${pad(p.hour)}:${pad(p.minute)}:${pad(p.second)}`;
}

/// "2026-01-01 08:00:00" in `zone`.
export function formatDateTime(unixMs: number, zone: Zone | null): string {
  const p = partsIn(unixMs, zone);
  return `${datePart(p)} ${clockPart(p)}`;
}

/// "08:00:00" when `unixMs` falls on the same day as `refMs` in `zone`, else
/// the date and time: cards stay short, and an old time still reads as old.
export function formatShort(unixMs: number, refMs: number, zone: Zone | null): string {
  const p = partsIn(unixMs, zone);
  return datePart(p) === datePart(partsIn(refMs, zone)) ? clockPart(p) : `${datePart(p)} ${clockPart(p)}`;
}

/// "11-03 20:06:10" when `unixMs` falls in the same year as `refMs` in
/// `zone`, else the full date: log lines stay narrow and an old year still
/// shows.
export function formatCompact(unixMs: number, refMs: number, zone: Zone | null): string {
  const p = partsIn(unixMs, zone);
  return p.year === partsIn(refMs, zone).year
    ? `${pad(p.month)}-${pad(p.day)} ${clockPart(p)}`
    : `${datePart(p)} ${clockPart(p)}`;
}

/// The zone's offset from UTC at `unixMs`, in whole seconds, read from the
/// wall time it shows then, so a named zone follows its daylight saving
/// time and an offset with seconds (some historical local mean times, or a
/// zone without a name) keeps them, as the watchdog's does.
export function offsetSecsAt(unixMs: number, zone: Zone | null): number {
  const p = partsIn(unixMs, zone);
  const wall = Date.UTC(p.year, p.month - 1, p.day, p.hour, p.minute, p.second);
  return Math.round((wall - Math.floor(unixMs / 1000) * 1000) / 1000);
}

/// "2026-11-03 21:30:05 +08:00": a time that still reads right away from
/// this machine, as in an export.
export function formatWithOffset(unixMs: number, zone: Zone | null): string {
  return `${formatDateTime(unixMs, zone)} ${offsetText(offsetSecsAt(unixMs, zone)).slice(3)}`;
}

/// "UTC+08:00", "UTC-03:30", and with seconds only when it has any,
/// "UTC+08:06:05", as the watchdog writes it (`utc_offset` in
/// `crates/wgal-runtime/src/notify/message.rs`).
export function offsetText(offsetSecs: number): string {
  const sign = offsetSecs < 0 ? "-" : "+";
  const abs = Math.abs(offsetSecs);
  const clock = `${pad(Math.floor(abs / 3600))}:${pad(Math.floor((abs % 3600) / 60))}`;
  return abs % 60 === 0 ? `UTC${sign}${clock}` : `UTC${sign}${clock}:${pad(abs % 60)}`;
}

/// The zone as the settings name it: its IANA name with the offset
/// (`time.zone_with_offset`), or the offset alone when the system zone has
/// no IANA name.
export function zoneText(status: TimeZoneStatus): string {
  const offset = offsetText(status.utc_offset_secs);
  return status.effective === null ? offset : t("time.zone_with_offset", { name: status.effective, offset });
}

/// Zone names this engine knows, for the zone picker. Empty where
/// `Intl.supportedValuesOf` is missing: the field still takes a typed name.
export function knownZones(): string[] {
  try {
    return typeof Intl.supportedValuesOf === "function" ? Intl.supportedValuesOf("timeZone") : [];
  } catch {
    return [];
  }
}

/// The window's own zone, for "follow the system" when another zone is set.
export function localZoneName(): string | null {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || null;
  } catch {
    return null;
  }
}
