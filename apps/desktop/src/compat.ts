// Whether this window and the background watchdog work together (owner's
// requests of 2026-11-03 and 2026-11-04, AGENTS.md "Versions"). The two
// versions are raised on their own and need not be equal. Each side has two
// minimums for the other: the oldest with which every function works (the
// snapshot's `host.min_gui_version`, this window's `MIN_WATCHDOG_VERSION`),
// and the oldest that can still use the tunnel functions at all
// (`host.min_usable_gui_version`, `MIN_USABLE_WATCHDOG_VERSION`). Below the
// first a side is too old for the full function; below the second it is not
// supported. A different but new enough version is normal and is never
// warned about. Used by the about tab, the header and the export.

import { t } from "./i18n";
import { MIN_USABLE_WATCHDOG_VERSION, MIN_WATCHDOG_VERSION, versionAtLeast } from "./protocol";

/// How far short of the other side's minimums a version is: `limited`,
/// below the full minimum but with the tunnel functions; `unsupported`,
/// below even those.
export type Shortfall = "limited" | "unsupported";

/// `unknown`: one side's version is not known now (`missing`: the watchdog's
/// while it is lost, or the window's own until the shell answers);
/// `incompatible`: `watchdog` and `gui` say how far short each side is (null
/// for one that is new enough), and `problems` says it in words that start
/// with ⚠, one per side.
export type Compatibility =
  | { kind: "unknown"; missing: "watchdog" | "gui" }
  | { kind: "compatible" }
  | { kind: "incompatible"; watchdog: Shortfall | null; gui: Shortfall | null; problems: string[] };

/// What this window needs of the watchdog; the defaults are this build's.
export interface WatchdogNeeds {
  minWatchdog?: string;
  usableWatchdog?: string;
}

/// The verdict when both sides are new enough.
export function compatibleText(): string {
  return t("compat.compatible");
}

export function watchdogShort(shortfall: Shortfall, minWatchdog: string): string {
  return t(shortfall === "limited" ? "compat.watchdog_limited" : "compat.watchdog_unsupported", { min: minWatchdog });
}

export function guiShort(shortfall: Shortfall, minGui: string): string {
  return t(shortfall === "limited" ? "compat.gui_limited" : "compat.gui_unsupported", { min: minGui });
}

/// How far `version` is from `full` and `usable`.
function shortfallOf(version: string, full: string, usable: string): Shortfall | null {
  if (!versionAtLeast(version, usable)) return "unsupported";
  if (!versionAtLeast(version, full)) return "limited";
  return null;
}

/// `gui` is the window's version, `watchdog`, `minGui` and `usableGui` the
/// snapshot's, each null while not known; `usableGui` is also null from a
/// watchdog older than 1.2.0, whose one minimum was the oldest it could work
/// with, so it stands for both. A watchdog below this window's minimums is
/// said even before the window's own version is known.
export function compatibility(
  gui: string | null,
  watchdog: string | null,
  minGui: string | null,
  usableGui: string | null,
  { minWatchdog = MIN_WATCHDOG_VERSION, usableWatchdog = MIN_USABLE_WATCHDOG_VERSION }: WatchdogNeeds = {},
): Compatibility {
  if (watchdog === null || minGui === null) return { kind: "unknown", missing: "watchdog" };
  const watchdogShortfall = shortfallOf(watchdog, minWatchdog, usableWatchdog);
  const guiShortfall = gui === null ? null : shortfallOf(gui, minGui, usableGui ?? minGui);
  const problems: string[] = [];
  if (watchdogShortfall !== null) problems.push(watchdogShort(watchdogShortfall, minWatchdog));
  if (guiShortfall !== null) problems.push(guiShort(guiShortfall, minGui));
  if (problems.length > 0) return { kind: "incompatible", watchdog: watchdogShortfall, gui: guiShortfall, problems };
  return gui === null ? { kind: "unknown", missing: "gui" } : { kind: "compatible" };
}

/// The verdict's lines, as the about tab shows them. `guiFailed`: the shell
/// did not say the window's version, so it will not be known.
export function compatibilityLines(c: Compatibility, guiFailed = false): string[] {
  switch (c.kind) {
    case "compatible":
      return [compatibleText()];
    case "incompatible":
      return c.problems;
    case "unknown":
      return [
        c.missing === "watchdog"
          ? t("compat.unknown_lost")
          : guiFailed
            ? t("compat.unknown_gui_failed")
            : t("compat.reading_gui"),
      ];
  }
}

/// The short warning every tab shows below the header while the two do not
/// work together; null otherwise, unknown included.
export function compatibilityWarning(c: Compatibility): string | null {
  return c.kind === "incompatible" ? c.problems.join(t("compat.separator")) : null;
}

/// What this window needs of the watchdog, in words: the full minimum, and
/// the usable one when it is older.
export function guiNeedsText(min: string, usable: string | null): string {
  return usable === null || usable === min
    ? t("about.gui_needs_value", { min })
    : t("about.gui_needs_both", { min, usable });
}

/// What the watchdog needs of a window, in words, as `guiNeedsText`.
export function watchdogNeedsText(min: string, usable: string | null): string {
  return usable === null || usable === min
    ? t("about.watchdog_needs_value", { min })
    : t("about.watchdog_needs_both", { min, usable });
}

/// Whether something a watchdog has from version `since` on is offered, for
/// the version of the latest current snapshot (null while lost or before
/// the first): one that is not plain `major.minor.patch` is not offered.
export function offeredSince(watchdogVersion: string | null, since: string): boolean {
  return (
    watchdogVersion !== null && /^\d{1,9}\.\d{1,9}\.\d{1,9}$/.test(watchdogVersion) && versionAtLeast(watchdogVersion, since)
  );
}

/// The watchdog version from which each tunnel has its own recovery retry
/// (`SetRecoveryRetry`, owner decision of 2026-11-04).
export const RECOVERY_RETRY_SINCE = "1.2.0";

/// The watchdog version from which the settings file can be exported and
/// imported (`ExportSettings`, `ImportSettings`).
export const SETTINGS_FILE_SINCE = "2.1.0";

/// The watchdog version from which agents may use WGAL through `wgal-mcp`
/// (`SetMcpEnabled`).
export const MCP_SINCE = "2.1.0";

/// The watchdog version from which the window can let agents read the chat
/// ID and change the device name, chat ID and bot token
/// (`SetMcpTelegramEnabled`, owner decision of 2026-11-06).
export const MCP_TELEGRAM_SINCE = "2.1.0";

/// The note the about tab puts after a version that falls short, as the
/// owner worded it; null for one that is new enough.
export function shortfallNote(shortfall: Shortfall | null): string | null {
  if (shortfall === null) return null;
  return t(shortfall === "limited" ? "about.limited" : "about.unsupported");
}
