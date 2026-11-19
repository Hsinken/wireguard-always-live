// Times written in the watchdog's configured zone, by IANA name or, when
// the system zone has none, by its offset from UTC.

import { describe, expect, it } from "vitest";

import { formatCompact, formatDateTime, formatShort, offsetText, zoneOf, zoneText } from "./time";

/// 2026-01-01 00:00:00 UTC.
const T = 1_767_225_600_000;

describe("times in the configured zone", () => {
  it("uses the effective IANA zone", () => {
    const taipei = zoneOf({ configured: null, effective: "Asia/Taipei", utc_offset_secs: 28_800 });
    expect(formatDateTime(T, taipei)).toBe("2026-01-01 08:00:00");
    const ny = zoneOf({ configured: "America/New_York", effective: "America/New_York", utc_offset_secs: -18_000 });
    expect(formatDateTime(T, ny)).toBe("2025-12-31 19:00:00");
    expect(formatDateTime(T + 1_234, zoneOf({ configured: "UTC", effective: "UTC", utc_offset_secs: 0 }))).toBe(
      "2026-01-01 00:00:01",
    );
  });

  it("follows daylight saving time by name, not by today's offset", () => {
    // 2026-07-01 12:00 UTC is 08:00 in New York (EDT, UTC-4), although the
    // reported offset (winter) is -5 h.
    const ny = zoneOf({ configured: "America/New_York", effective: "America/New_York", utc_offset_secs: -18_000 });
    expect(formatDateTime(Date.UTC(2026, 6, 1, 12), ny)).toBe("2026-07-01 08:00:00");
  });

  it("falls back to the offset when the system zone has no IANA name", () => {
    expect(formatDateTime(T, zoneOf({ configured: null, effective: null, utc_offset_secs: 28_800 }))).toBe(
      "2026-01-01 08:00:00",
    );
    expect(formatDateTime(T, zoneOf({ configured: null, effective: null, utc_offset_secs: -12_600 }))).toBe(
      "2025-12-31 20:30:00",
    );
    // A name this engine does not know uses the offset too, never throws.
    expect(formatDateTime(T, { name: "Not/A_Zone", offsetSecs: 3_600 })).toBe("2026-01-01 01:00:00");
  });

  it("writes only the clock for times on the same day", () => {
    const taipei = zoneOf({ configured: null, effective: "Asia/Taipei", utc_offset_secs: 28_800 });
    expect(formatShort(T + 60_000, T, taipei)).toBe("08:01:00");
    // 2025-12-31 23:59 in Taipei is the day before.
    expect(formatShort(T - 9 * 3_600_000 + 60_000, T, taipei)).toBe("2025-12-31 23:01:00");
  });

  it("writes log times without this year", () => {
    const taipei = zoneOf({ configured: null, effective: "Asia/Taipei", utc_offset_secs: 28_800 });
    expect(formatCompact(T + 60_000, T, taipei)).toBe("01-01 08:01:00");
    // 2025-12-31 in Taipei is last year there.
    expect(formatCompact(T - 9 * 3_600_000, T, taipei)).toBe("2025-12-31 23:00:00");
  });

  it("names the zone with its offset", () => {
    expect(offsetText(28_800)).toBe("UTC+08:00");
    expect(offsetText(-12_600)).toBe("UTC-03:30");
    expect(offsetText(0)).toBe("UTC+00:00");
    // Seconds only when the offset has any, as the watchdog writes them.
    expect(offsetText(8 * 3600 + 6 * 60 + 5)).toBe("UTC+08:06:05");
    expect(offsetText(-(3 * 3600 + 30 * 60 + 5))).toBe("UTC-03:30:05");
    expect(zoneText({ configured: null, effective: "Asia/Taipei", utc_offset_secs: 28_800 })).toBe("Asia/Taipei（UTC+08:00）");
    expect(zoneText({ configured: null, effective: null, utc_offset_secs: 19_800 })).toBe("UTC+05:30");
  });
});
