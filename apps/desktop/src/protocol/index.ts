// TypeScript side of the shared contract. Types are generated from the Rust
// crate `wgal-protocol` into ./generated (do not edit those files); the
// constants below mirror the Rust ones and are checked against the fixtures
// (`protocol_info.json`, `limits.json`) in contract.test.ts.

import type { NotificationTemplates } from "./generated/NotificationTemplates";
import type { ProbeTiming } from "./generated/ProbeTiming";
import type { RecoveryRetry } from "./generated/RecoveryRetry";

export const PROTOCOL_VERSION = 1;
/** The settings file version this contract reads and writes. */
export const SETTINGS_DOCUMENT_VERSION = 1;
export const MAX_MESSAGE_BYTES = 64 * 1024;

/** Most tunnels the watchdog registers and a snapshot carries. */
export const MAX_TUNNELS = 16;
/** Most tunnels one discovery result lists. */
export const MAX_DISCOVERED = 32;
/** Most observe-only targets per tunnel, besides its recovery target. */
export const MAX_OBSERVE_TARGETS = 4;
/** Most entries in one event log page. */
export const MAX_EVENT_PAGE = 100;
/** Range of a configured worker thread count (`WorkerCount`). */
export const MIN_WORKERS = 2;
export const MAX_WORKERS = 17;

/** Inclusive ranges of the `ProbeTiming` fields. */
export const PROBE_INTERVAL_SECS = { min: 10, max: 3600 } as const;
export const PROBE_TIMEOUT_SECS = { min: 1, max: 10 } as const;
export const PROBE_RETRIES = { min: 0, max: 10 } as const;
export const PROBE_MIN_FAULT_SECS = { min: 0, max: 3600 } as const;

/** The timing a newly registered tunnel gets. */
export const DEFAULT_PROBE_TIMING: Readonly<ProbeTiming> = {
  interval_secs: 15,
  timeout_secs: 2,
  retries: 2,
  min_fault_secs: 60,
};

/**
 * Inclusive ranges of the `RecoveryRetry` fields: rounds after the first (a
 * `null` limit keeps trying) and the wait between them.
 */
export const RECOVERY_RETRY_LIMIT = { min: 0, max: 3600 } as const;
export const RECOVERY_RETRY_INTERVAL_SECS = { min: 10, max: 10800 } as const;

/** The retry a newly registered tunnel gets: 100 more rounds, one a minute. */
export const DEFAULT_RECOVERY_RETRY: Readonly<RecoveryRetry> = { limit: 100, interval_secs: 60 };

/**
 * The placeholders each `NotificationTemplates` text must use, each exactly
 * once, and the only ones it may (`{device}` and so on; `{{` and `}}` are
 * literal braces).
 */
export const TEMPLATE_PLACEHOLDERS = {
  formal: ["device", "tunnel", "incident", "time_zone", "started_at", "result", "retry"],
  result_failed: [],
  result_not_executed: [],
  retry_limited: ["interval", "limit"],
  retry_forever: ["interval"],
  retry_none: [],
  exhausted: ["device", "tunnel", "incident", "time_zone", "started_at", "retries"],
  recovered: ["device", "tunnel", "incident", "time_zone", "started_at", "recovered_at"],
  test: ["device", "time_zone", "time"],
  unnamed_device: [],
} as const satisfies Record<Exclude<keyof NotificationTemplates, "id">, readonly string[]>;

/**
 * The oldest GUI with every function of the watchdog is in every snapshot
 * (`host.min_gui_version`), and the oldest that can still use its tunnel
 * functions (`host.min_usable_gui_version`); these mirror the Rust
 * `MIN_GUI_VERSION` and `MIN_USABLE_GUI_VERSION` of this contract build and
 * are checked against `limits.json`. 3.0.1: the event log may name the
 * `set_cli_language` request (watchdog 4.0.0, owner decision of 2026-11-19).
 * 3.1.0: the snapshot carries the host name messages give when no device
 * name is set (`notification.host_name`, watchdog 4.1.0), which the GUI
 * shows (owner decision of 2026-11-19). 4.2.1: the window, the watchdog,
 * wgal-mcp and wgal share one version from 4.2.1, and every minimum starts
 * there (owner decision of 2026-11-19, a one-time sync).
 */
export const MIN_GUI_VERSION = "4.2.1";
export const MIN_USABLE_GUI_VERSION = "4.2.1";

/**
 * The oldest watchdog with which every function of this GUI works (owner
 * decision of 2026-11-04: a minimum is what makes every function work, not
 * what can merely connect). Raise it whenever the GUI uses something older
 * watchdogs do not have; wording, a new language or a fix on either side
 * never needs it. 2.0.0: WGAL started again at 2.0.0 before it had users
 * (owner decision of 2026-11-06), so no 1.x watchdog is supported. 2.1.0:
 * this GUI sends device names of up to 64 characters (`DeviceName`), which
 * a 2.0.0 watchdog refuses beyond 96 bytes; 2.0.0 was never installed.
 * 4.1.0: the device name field shows the host name messages give while it
 * is empty, which only a 4.1.0 watchdog reports (owner decision of
 * 2026-11-19). 4.2.1: one version for the whole set from 4.2.1, every
 * minimum starting there (owner decision of 2026-11-19, a one-time sync).
 */
export const MIN_WATCHDOG_VERSION = "4.2.1";

/**
 * The oldest watchdog with which this GUI can still use the tunnel
 * functions at all (read its snapshot, connect, disconnect, monitor). At
 * most `MIN_WATCHDOG_VERSION`; 2.1.0, as above: this GUI decodes a device
 * name as `DeviceName`, so a 2.0.0 watchdog's longer names would not read.
 * 4.2.1: the one-time sync above.
 */
export const MIN_USABLE_WATCHDOG_VERSION = "4.2.1";

/**
 * How long the parts of a Bot Token and a Chat ID may be (`limits.json`):
 * Telegram's own, with 5 characters to spare either way (owner decision of
 * 2026-11-06).
 */
export const BOT_ID_DIGITS = { min: 3, max: 15 } as const;
export const BOT_SECRET_CHARS = { min: 30, max: 40 } as const;
export const CHAT_ID_DIGITS = { min: 1, max: 19 } as const;
export const CHAT_USERNAME_CHARS = { min: 1, max: 37 } as const;

/**
 * Most characters a device name has (`DeviceName`, `limits.json`); a longer
 * one is cut to three fewer and "..." (owner decision of 2026-11-06).
 */
export const DEVICE_NAME_MAX_CHARS = 64;

/** The dot-separated numbers a version starts with, before any `-` or `+`. */
function versionNumbers(version: string): number[] {
  const core = version.split(/[-+]/, 1)[0] ?? "";
  const numbers: number[] = [];
  for (const part of core.split(".")) {
    if (!/^\d+$/.test(part)) break;
    numbers.push(Number(part));
  }
  return numbers;
}

/**
 * Whether `version` is `minimum` or newer, as Rust's
 * `ProductVersion::is_at_least`: compares the leading numbers (`1.10.0` is
 * newer than `1.9.3`; a missing number counts as 0) and ignores a suffix.
 */
export function versionAtLeast(version: string, minimum: string): boolean {
  const have = versionNumbers(version);
  const need = versionNumbers(minimum);
  for (let i = 0; i < Math.max(have.length, need.length); i += 1) {
    const a = have[i] ?? 0;
    const b = need[i] ?? 0;
    if (a !== b) return a > b;
  }
  return true;
}

export type { AdapterCapabilities } from "./generated/AdapterCapabilities";
export type { AdapterErrorKind } from "./generated/AdapterErrorKind";
export type { BotToken } from "./generated/BotToken";
export type { Capability } from "./generated/Capability";
export type { ChatId } from "./generated/ChatId";
export type { CredentialState } from "./generated/CredentialState";
export type { DataHealth } from "./generated/DataHealth";
export type { DesiredState } from "./generated/DesiredState";
export type { Detail } from "./generated/Detail";
export type { DiscoveredTunnel } from "./generated/DiscoveredTunnel";
export type { DisplayName } from "./generated/DisplayName";
export type { ErrorCode } from "./generated/ErrorCode";
export type { ErrorReason } from "./generated/ErrorReason";
export type { EventEntry } from "./generated/EventEntry";
export type { Fault } from "./generated/Fault";
export type { FirstRecovery } from "./generated/FirstRecovery";
export type { HostSettings } from "./generated/HostSettings";
export type { HostStatus } from "./generated/HostStatus";
export type { IncidentId } from "./generated/IncidentId";
export type { IncidentStatus } from "./generated/IncidentStatus";
export type { Light } from "./generated/Light";
export type { LightReason } from "./generated/LightReason";
export type { LogEvent } from "./generated/LogEvent";
export type { MessageTemplate } from "./generated/MessageTemplate";
export type { MonitorSettings } from "./generated/MonitorSettings";
export type { NotificationFailure } from "./generated/NotificationFailure";
export type { NotificationState } from "./generated/NotificationState";
export type { NotificationStatus } from "./generated/NotificationStatus";
export type { NotificationTemplates } from "./generated/NotificationTemplates";
export type { OsState } from "./generated/OsState";
export type { Outcome } from "./generated/Outcome";
export type { ProbeKind } from "./generated/ProbeKind";
export type { ProbeResult } from "./generated/ProbeResult";
export type { ProbeSummary } from "./generated/ProbeSummary";
export type { ProbeTarget } from "./generated/ProbeTarget";
export type { ProbeTiming } from "./generated/ProbeTiming";
export type { NoticeKind } from "./generated/NoticeKind";
export type { AutoRecoveryImport } from "./generated/AutoRecoveryImport";
export type { HostSettingsEntry } from "./generated/HostSettingsEntry";
export type { ImportItem } from "./generated/ImportItem";
export type { ImportMatch } from "./generated/ImportMatch";
export type { ImportOptions } from "./generated/ImportOptions";
export type { ImportOutcome } from "./generated/ImportOutcome";
export type { ImportResult } from "./generated/ImportResult";
export type { SettingsDocument } from "./generated/SettingsDocument";
export type { SettingsFormat } from "./generated/SettingsFormat";
export type { TunnelSettingsEntry } from "./generated/TunnelSettingsEntry";
export type { RecoveredNotification } from "./generated/RecoveredNotification";
export type { RecoveryRetry } from "./generated/RecoveryRetry";
export type { ProtocolError } from "./generated/ProtocolError";
export type { ProtocolInfo } from "./generated/ProtocolInfo";
export type { RecoveryPhase } from "./generated/RecoveryPhase";
export type { Request } from "./generated/Request";
export type { RequestEnvelope } from "./generated/RequestEnvelope";
export type { RequestId } from "./generated/RequestId";
export type { Response } from "./generated/Response";
export type { ResponseEnvelope } from "./generated/ResponseEnvelope";
export type { Snapshot } from "./generated/Snapshot";
export type { StorageHealth } from "./generated/StorageHealth";
export type { Takeover } from "./generated/Takeover";
export type { TargetAddress } from "./generated/TargetAddress";
export type { TargetStatus } from "./generated/TargetStatus";
export type { TemplatesId } from "./generated/TemplatesId";
export type { TestNotificationState } from "./generated/TestNotificationState";
export type { TestNotificationStatus } from "./generated/TestNotificationStatus";
export type { TimeZoneName } from "./generated/TimeZoneName";
export type { TimeZoneStatus } from "./generated/TimeZoneStatus";
export type { TunnelId } from "./generated/TunnelId";
export type { TunnelStatus } from "./generated/TunnelStatus";
export type { UserIntent } from "./generated/UserIntent";
export type { WorkerCount } from "./generated/WorkerCount";
export type { WorkerStatus } from "./generated/WorkerStatus";
