// The settings tab without the DOM: what the watchdog reports about its
// worker threads and time zone, worded, and the form read back into the
// `HostSettings` of `Request::UpdateHostSettings`.

import { MCP_TELEGRAM_SINCE, offeredSince } from "./compat";
import { type Parsed, parseTimeZoneName, parseWorkers } from "./forms";
import { t } from "./i18n";
import { MIN_WORKERS, type HostSettings, type Snapshot } from "./protocol";
import { zoneText } from "./time";
import type { Availability } from "./view";

export interface HostView {
  /// Whether the form can be sent: not while the watchdog is lost.
  save: Availability;
  paused: boolean;
  workers: {
    auto: boolean;
    /// What the number field starts with: the configured count, else the
    /// suggestion.
    count: string;
    /// The automatic choice's label with the suggestion.
    autoLabel: string;
    /// Bounds of the number field.
    min: number;
    max: number;
    /// Suggested, limit, effective and running, as label and value.
    facts: Array<[string, string]>;
  };
  zone: {
    follow: boolean;
    /// What the name field starts with: the configured zone, else the
    /// effective one.
    name: string;
    followLabel: string;
    current: string;
  };
}

/// The switch that lets agents set Telegram, under the MCP switch (owner
/// decision of 2026-11-06): what it is, and the button that turns it the
/// other way. It keeps its setting while MCP is off, where it has no effect
/// and cannot be changed (greyed out).
export interface McpTelegramView {
  state: string;
  toggle: string;
  control: Availability;
}

export function mcpTelegramView(snapshot: Snapshot | null, lost: boolean): McpTelegramView {
  if (lost || snapshot === null || !offeredSince(snapshot.host.watchdog_version, MCP_TELEGRAM_SINCE)) {
    const why = t("mcp.telegram_not_offered");
    return { state: why, toggle: t("mcp.telegram_allow"), control: { kind: "disabled", why } };
  }
  const on = snapshot.host.mcp_telegram_enabled;
  const state = on ? t("mcp.telegram_on") : t("mcp.telegram_off");
  const toggle = on ? t("mcp.telegram_disallow") : t("mcp.telegram_allow");
  if (!snapshot.host.mcp_enabled) {
    return {
      state: t("mcp.telegram_inactive", { state }),
      toggle,
      control: { kind: "disabled", why: t("mcp.telegram_needs_mcp") },
    };
  }
  return { state, toggle, control: { kind: "enabled" } };
}

export function hostView(snapshot: Snapshot | null, lost: boolean, localZone: string | null): HostView | null {
  if (snapshot === null) return null;
  const w = snapshot.host.workers;
  const tz = snapshot.host.time_zone;
  const monitored = snapshot.monitoring_paused
    ? 0
    : snapshot.tunnels.filter((t) => t.intent.monitoring_enabled).length;
  return {
    save: lost ? { kind: "disabled", why: t("common.lost") } : { kind: "enabled" },
    paused: snapshot.monitoring_paused,
    workers: {
      auto: w.configured === null,
      count: String(w.configured ?? w.suggested),
      autoLabel: t("settings.workers_auto_suggested", { count: w.suggested }),
      min: MIN_WORKERS,
      max: Math.max(w.limit, w.configured ?? 0, MIN_WORKERS),
      facts: [
        [t("settings.fact_suggested"), `${w.suggested}`],
        [t("settings.fact_limit"), t("settings.fact_limit_value", { limit: w.limit, monitored, min: MIN_WORKERS })],
        [t("settings.fact_effective"), `${w.effective}`],
        [t("settings.fact_running"), t("settings.fact_running_value", { count: w.running })],
      ],
    },
    zone: {
      follow: tz.configured === null,
      name: tz.configured ?? tz.effective ?? "",
      followLabel:
        tz.configured === null
          ? t("settings.zone_follow_current", { zone: zoneText(tz) })
          : localZone === null
            ? t("settings.zone_follow")
            : t("settings.zone_follow_local", { zone: localZone }),
      current: t("settings.zone_in_use", { zone: zoneText(tz) }),
    },
  };
}

/// The form as typed.
export interface HostInput {
  workersAuto: boolean;
  workers: string;
  zoneFollow: boolean;
  zone: string;
}

export function parseHostSettings(input: HostInput, snapshot: Snapshot): Parsed<HostSettings> {
  let workers: number | null = null;
  if (!input.workersAuto) {
    const parsed = parseWorkers(input.workers, snapshot.host.workers.limit, snapshot.host.workers.configured);
    if (!parsed.ok) return parsed;
    workers = parsed.value;
  }
  let zone: string | null = null;
  if (!input.zoneFollow) {
    const parsed = parseTimeZoneName(input.zone);
    if (!parsed.ok) return parsed;
    zone = parsed.value;
  }
  return { ok: true, value: { workers, time_zone: zone } };
}
