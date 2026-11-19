// The about tab without the DOM (owner's requests of 2026-11-03 and
// 2026-11-04): which build of the window and of the background watchdog
// runs, so the owner can check them, the oldest versions each needs of the
// other, and whether they work together (`compat.ts`). A version that falls
// short of the other side's minimums says so after it: too old for the full
// function, or not supported at all. The window's version comes from the
// desktop binary itself (`app_version`); the watchdog's, and the oldest
// windows it works with, from its latest snapshot, and only while that is
// current. The MCP server is built and installed with the watchdog, so it
// shares the watchdog's row (owner request of 2026-11-06): the row says
// more only when the version the `wgal-mcp` executable beside the window
// carries, as the shell reads it without running it (`mcp_version`),
// differs, or it is missing or cannot be read.

import {
  type Compatibility,
  type Shortfall,
  compatibility,
  compatibilityLines,
  guiNeedsText,
  shortfallNote,
  watchdogNeedsText,
} from "./compat";
import { t } from "./i18n";
import { MIN_USABLE_WATCHDOG_VERSION, MIN_WATCHDOG_VERSION, PROTOCOL_VERSION, type Snapshot } from "./protocol";
import { platformOf } from "./report";

/// What the shell says of the MCP server installed beside the window.
export type McpServer = { state: "installed"; version: string } | { state: "missing" } | { state: "unreadable" };

/// The shell's answer, checked: anything else is `null`. A version is what
/// a product version may be (`ProductVersion`): 1 to 32 ASCII letters,
/// digits and `. + -`, starting with a digit.
export function asMcpServer(value: unknown): McpServer | null {
  if (typeof value !== "object" || value === null) return null;
  const v = value as Record<string, unknown>;
  switch (v.state) {
    case "missing":
    case "unreadable":
      return { state: v.state };
    case "installed":
      return typeof v.version === "string" && /^[0-9][0-9A-Za-z.+-]{0,31}$/.test(v.version)
        ? { state: "installed", version: v.version }
        : null;
    default:
      return null;
  }
}

/// Where the installation puts `wgal-mcp`, beside the window, on each
/// platform (`packaging/<os>/install.*`): what an agent's MCP settings run.
export const MCP_COMMANDS: Readonly<Record<string, string>> = {
  Windows: String.raw`C:\Program Files\WGAL\wgal-mcp.exe`,
  Linux: "/opt/wgal/wgal-mcp",
  macOS: "/Applications/WG Always Live.app/Contents/MacOS/wgal-mcp",
};

/// The MCP server entry an agent's settings need on `platform` (as
/// `platformOf` names it); Windows' for a platform it does not name.
export function mcpConfig(platform: string): string {
  const command = MCP_COMMANDS[platform] ?? String.raw`C:\Program Files\WGAL\wgal-mcp.exe`;
  return JSON.stringify({ mcpServers: { wgal: { command, args: [] } } }, null, 2);
}

/// The MCP server's version in words: reading until the shell answers, then
/// the version, not installed, or unavailable. The server is built and
/// installed with the watchdog, so a version that differs from the
/// watchdog's, when that is known, says so.
export function mcpServerText(mcp: McpServer | null, failed: boolean, watchdogVersion: string | null): string {
  if (mcp === null) return failed ? t("about.unavailable") : t("about.reading");
  switch (mcp.state) {
    case "installed":
      return watchdogVersion !== null && watchdogVersion !== mcp.version
        ? t("about.version_with_note", { version: mcp.version, note: t("about.mcp_differs") })
        : mcp.version;
    case "missing":
      return t("about.mcp_missing");
    case "unreadable":
      return t("about.unavailable");
  }
}

/// What a fact from the watchdog says while it is lost.
export function watchdogUnknown(): string {
  return t("about.watchdog_unknown");
}

export interface AboutView {
  facts: Array<[string, string]>;
  compatibility: Compatibility;
  /// The verdict in words, one line per problem.
  lines: string[];
}

/// The watchdog's version, with how far short it falls and a note when the
/// MCP server beside the window is not the same version, is missing or
/// cannot be read, both in one pair of brackets; no MCP note while the
/// shell has not answered yet.
function watchdogAndMcpText(
  version: string,
  shortfall: Shortfall | null,
  mcp: McpServer | null,
  mcpFailed: boolean,
): string {
  const notes = [shortfallNote(shortfall), mcpNote(mcp, mcpFailed, version)].filter((n) => n !== null);
  return notes.length === 0
    ? version
    : t("about.version_with_note", { version, note: notes.join(t("compat.separator")) });
}

/// What the watchdog's row says of the MCP server beside the window, if
/// anything: nothing while the shell reads or when the versions match.
function mcpNote(mcp: McpServer | null, failed: boolean, watchdogVersion: string): string | null {
  if (mcp === null) return failed ? t("about.mcp_server_unreadable") : null;
  switch (mcp.state) {
    case "installed":
      return mcp.version === watchdogVersion ? null : t("about.mcp_server_differs", { version: mcp.version });
    case "missing":
      return t("about.mcp_server_missing");
    case "unreadable":
      return t("about.mcp_server_unreadable");
  }
}

/// A version with the note on how far short it falls, if it does.
function versionWithNote(version: string, shortfall: Shortfall | null): string {
  const note = shortfallNote(shortfall);
  return note === null ? version : t("about.version_with_note", { version, note });
}

/// `guiVersion` is null until the shell has answered, and stays null with
/// `guiFailed` when it could not; `mcp` and `mcpFailed` the same for the
/// MCP server. Nothing from a snapshot is used while the watchdog is lost.
export function aboutView(
  snapshot: Snapshot | null,
  lost: boolean,
  guiVersion: string | null,
  userAgent: string,
  {
    guiFailed = false,
    mcp = null,
    mcpFailed = false,
    minWatchdog = MIN_WATCHDOG_VERSION,
    usableWatchdog = MIN_USABLE_WATCHDOG_VERSION,
  }: {
    guiFailed?: boolean;
    mcp?: McpServer | null;
    mcpFailed?: boolean;
    minWatchdog?: string;
    usableWatchdog?: string;
  } = {},
): AboutView {
  const host = snapshot !== null && !lost ? snapshot.host : null;
  const c = compatibility(
    guiVersion,
    host?.watchdog_version ?? null,
    host?.min_gui_version ?? null,
    host?.min_usable_gui_version ?? null,
    { minWatchdog, usableWatchdog },
  );
  const short = c.kind === "incompatible" ? c : { watchdog: null, gui: null };
  const gui = guiVersion === null ? (guiFailed ? t("about.unavailable") : t("about.reading")) : versionWithNote(guiVersion, short.gui);
  return {
    facts: [
      [t("about.name"), "WG Always Live"],
      [t("about.gui_version"), gui],
      [
        t("about.watchdog_version"),
        host === null ? watchdogUnknown() : watchdogAndMcpText(host.watchdog_version, short.watchdog, mcp, mcpFailed),
      ],
      [t("about.gui_needs"), guiNeedsText(minWatchdog, usableWatchdog)],
      [
        t("about.watchdog_needs"),
        host === null ? watchdogUnknown() : watchdogNeedsText(host.min_gui_version, host.min_usable_gui_version),
      ],
      [t("about.protocol"), String(PROTOCOL_VERSION)],
      [t("about.platform"), platformOf(userAgent)],
    ],
    compatibility: c,
    lines: compatibilityLines(c, guiFailed),
  };
}
