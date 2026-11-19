// The 關於 tab: both versions, both minimums, the protocol version and the
// platform, and whether the window and the watchdog work together. They are
// compatible when each is at least the other's minimum, whatever the two
// versions are; below the full minimum a side is too old for the full
// function, below the usable one it is not supported (owner decision of
// 2026-11-04); nothing is judged while the watchdog is lost.

import { describe, expect, it } from "vitest";

import { MCP_COMMANDS, aboutView, asMcpServer, mcpConfig, mcpServerText, watchdogUnknown } from "./about";
import { compatibility, compatibleText, compatibilityLines, compatibilityWarning } from "./compat";
import { MIN_USABLE_WATCHDOG_VERSION, MIN_WATCHDOG_VERSION, PROTOCOL_VERSION, type Snapshot } from "./protocol";

const WINDOWS_UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Edg/140.0";

function snapshot(
  watchdog_version: string,
  min_gui_version = "1.0.0",
  min_usable_gui_version: string | null = null,
): Snapshot {
  return { host: { watchdog_version, min_gui_version, min_usable_gui_version } } as unknown as Snapshot;
}

describe("the about tab", () => {
  it("shows both versions, both minimums, the protocol and the platform", () => {
    // The minimums are given, so raising only the full one (AGENTS.md
    // "Versions") never breaks this test (FIX-06, AR-17); the case of two
    // different minimums is below.
    const needs = { minWatchdog: "2.1.0", usableWatchdog: "2.1.0" };
    const view = aboutView(snapshot("2.1.5"), false, "2.1.0", WINDOWS_UA, needs);
    expect(view.facts).toEqual([
      ["程式名稱", "WG Always Live"],
      ["介面版本", "2.1.0"],
      ["WatchDog / MCP 伺服器 版本", "2.1.5"],
      // Both minimums are 2.1.0, so one is said.
      ["介面的需求", "需要 WatchDog ≥ 2.1.0"],
      ["WatchDog 的需求", "WatchDog 需要介面 ≥ 1.0.0"],
      ["通訊協定版本", String(PROTOCOL_VERSION)],
      ["平台", "Windows"],
    ]);
  });

  it("says this window's own minimums unless others are given", () => {
    const given = aboutView(snapshot("2.1.5"), false, "2.1.0", WINDOWS_UA, {
      minWatchdog: MIN_WATCHDOG_VERSION,
      usableWatchdog: MIN_USABLE_WATCHDOG_VERSION,
    });
    expect(aboutView(snapshot("2.1.5"), false, "2.1.0", WINDOWS_UA).facts[3]).toEqual(given.facts[3]);
  });

  it("shares the watchdog's row with the MCP server, and says when the server differs", () => {
    const row = (server: Parameters<typeof aboutView>[4], watchdog = "9.0.0") =>
      aboutView(snapshot(watchdog), false, "1.2.0", WINDOWS_UA, server).facts[2];
    const label = "WatchDog / MCP 伺服器 版本";
    expect(row({ mcp: { state: "installed", version: "9.0.0" } })).toEqual([label, "9.0.0"]);
    expect(row({}), "nothing while the shell reads").toEqual([label, "9.0.0"]);
    expect(row({ mcp: { state: "installed", version: "8.9.9" } })).toEqual([
      label,
      "9.0.0（MCP 伺服器為 8.9.9，與 WatchDog 版本不同）",
    ]);
    expect(row({ mcp: { state: "missing" } })).toEqual([label, "9.0.0（MCP 伺服器未安裝）"]);
    expect(row({ mcp: { state: "unreadable" } })).toEqual([label, "9.0.0（無法取得 MCP 伺服器版本）"]);
    expect(row({ mcpFailed: true })).toEqual([label, "9.0.0（無法取得 MCP 伺服器版本）"]);
    // A watchdog too old for this window and a differing server share one
    // pair of brackets.
    expect(row({ mcp: { state: "installed", version: "1.0.0" } }, "1.1.0")).toEqual([
      label,
      "1.1.0（版本不支援-請升級；MCP 伺服器為 1.0.0，與 WatchDog 版本不同）",
    ]);
    expect(row({ mcp: { state: "installed", version: "1.1.0" } }, "1.1.0")).toEqual([
      label,
      "1.1.0（版本不支援-請升級）",
    ]);
    // Nothing is compared while the watchdog is lost.
    const lost = aboutView(snapshot("9.0.0"), true, "1.2.0", WINDOWS_UA, {
      mcp: { state: "installed", version: "8.9.9" },
    }).facts[2];
    expect(lost).toEqual([label, "無法取得（背景監控失聯）"]);
    // The MCP settings still show the server's own version.
    expect(mcpServerText(null, false, null)).toBe("讀取中…");
  });

  it("takes only a well-formed answer from the shell", () => {
    expect(asMcpServer({ state: "installed", version: "1.2.1" })).toEqual({ state: "installed", version: "1.2.1" });
    expect(asMcpServer({ state: "missing" })).toEqual({ state: "missing" });
    expect(asMcpServer({ state: "unreadable", extra: 1 })).toEqual({ state: "unreadable" });
    for (const odd of [
      null,
      "1.2.1",
      {},
      { state: "installed" },
      { state: "installed", version: "v1.2" },
      { state: "installed", version: "<b>1</b>" },
      { state: "installed", version: "1".repeat(33) },
      { state: "other" },
    ]) {
      expect(asMcpServer(odd), JSON.stringify(odd)).toBeNull();
    }
  });

  it("calls a 2.1.0 window with a 4.2.5 watchdog compatible, without a warning", () => {
    const view = aboutView(snapshot("4.2.5"), false, "2.1.0", WINDOWS_UA);
    expect(view.compatibility).toEqual({ kind: "compatible" });
    expect(view.lines).toEqual([compatibleText()]);
    expect(compatibleText()).toBe("✓ 相容");
    expect(compatibilityWarning(view.compatibility)).toBeNull();
    // Newer on either side, by any number, is still fine.
    for (const [gui, watchdog] of [
      ["1.0.0", "1.0.0"],
      ["1.3.0", "1.0.0"],
      ["1.0.0", "2.0.0"],
      ["1.10.0", "1.9.3"],
    ] as const) {
      expect(
        compatibility(gui, watchdog, "1.0.0", "1.0.0", { minWatchdog: "1.0.0", usableWatchdog: "1.0.0" }).kind,
        `${gui} / ${watchdog}`,
      ).toBe("compatible");
    }
  });

  it("says a window below the watchdog's full minimum is too old for the full function", () => {
    const view = aboutView(snapshot("4.2.4", "1.2.0", "1.0.0"), false, "1.1.9", WINDOWS_UA);
    expect(view.facts[1]).toEqual(["介面版本", "1.1.9（版本過舊-無法支援完整功能，建議升級）"]);
    expect(view.facts[2]).toEqual(["WatchDog / MCP 伺服器 版本", "4.2.4"]);
    expect(view.facts[4]).toEqual(["WatchDog 的需求", "完整功能需要介面 ≥ 1.2.0；隧道功能需要 ≥ 1.0.0"]);
    expect(view.lines).toEqual(["⚠ 介面版本過舊-無法支援完整功能，建議升級（WatchDog 需要 1.2.0 以上）"]);
    expect(compatibilityWarning(view.compatibility)).toBe(
      "⚠ 介面版本過舊-無法支援完整功能，建議升級（WatchDog 需要 1.2.0 以上）",
    );
    expect(view.compatibility).toMatchObject({ kind: "incompatible", gui: "limited", watchdog: null });
  });

  it("says a window below even the usable minimum is not supported", () => {
    const view = aboutView(snapshot("4.2.4", "1.2.0", "1.2.0"), false, "1.1.9", WINDOWS_UA);
    expect(view.facts[1]).toEqual(["介面版本", "1.1.9（版本不支援-請升級）"]);
    expect(view.facts[4]).toEqual(["WatchDog 的需求", "WatchDog 需要介面 ≥ 1.2.0"]);
    expect(view.lines).toEqual(["⚠ 介面版本不支援-請升級（WatchDog 需要 1.2.0 以上）"]);
    expect(view.compatibility).toMatchObject({ kind: "incompatible", gui: "unsupported" });
  });

  it("takes an older watchdog's one minimum for both", () => {
    // A watchdog before 1.2.0 sends no usable minimum: its one minimum was
    // the oldest window it could work with at all.
    const view = aboutView(snapshot("1.1.0", "1.1.0", null), false, "1.0.9", WINDOWS_UA, {
      minWatchdog: "1.0.0",
      usableWatchdog: "1.0.0",
    });
    expect(view.facts[1]).toEqual(["介面版本", "1.0.9（版本不支援-請升級）"]);
    expect(view.facts[4]).toEqual(["WatchDog 的需求", "WatchDog 需要介面 ≥ 1.1.0"]);
  });

  it("says how far a watchdog is from what this window needs", () => {
    const needs = { minWatchdog: "1.2.0", usableWatchdog: "1.0.0" };
    const limited = aboutView(snapshot("1.1.0"), false, "1.2.0", WINDOWS_UA, needs);
    expect(limited.facts[2]).toEqual(["WatchDog / MCP 伺服器 版本", "1.1.0（版本過舊-無法支援完整功能，建議升級）"]);
    expect(limited.facts[3]).toEqual(["介面的需求", "完整功能需要 WatchDog ≥ 1.2.0；隧道功能需要 ≥ 1.0.0"]);
    expect(limited.lines).toEqual(["⚠ WatchDog 版本過舊-無法支援完整功能，建議升級（此介面需要 1.2.0 以上）"]);
    const unsupported = aboutView(snapshot("0.9.0"), false, "1.2.0", WINDOWS_UA, needs);
    expect(unsupported.facts[2]).toEqual(["WatchDog / MCP 伺服器 版本", "0.9.0（版本不支援-請升級）"]);
    expect(unsupported.lines).toEqual(["⚠ WatchDog 版本不支援-請升級（此介面需要 1.2.0 以上）"]);
    // With this build's minimums too, and before the window's own version is known.
    const early = aboutView(snapshot("0.9.0"), false, null, WINDOWS_UA);
    expect(early.lines).toEqual([`⚠ WatchDog 版本不支援-請升級（此介面需要 ${MIN_WATCHDOG_VERSION} 以上）`]);
    // Both at once: each on its own line, joined for the header.
    const both = compatibility("1.0.0", "1.0.0", "1.2.0", "1.0.0", { minWatchdog: "1.1.0", usableWatchdog: "1.0.0" });
    expect(compatibilityLines(both)).toEqual([
      "⚠ WatchDog 版本過舊-無法支援完整功能，建議升級（此介面需要 1.1.0 以上）",
      "⚠ 介面版本過舊-無法支援完整功能，建議升級（WatchDog 需要 1.2.0 以上）",
    ]);
    expect(compatibilityWarning(both)).toBe(
      "⚠ WatchDog 版本過舊-無法支援完整功能，建議升級（此介面需要 1.1.0 以上）；⚠ 介面版本過舊-無法支援完整功能，建議升級（WatchDog 需要 1.2.0 以上）",
    );
  });

  it("judges nothing while the watchdog is lost", () => {
    for (const [s, lost] of [
      [null, true],
      [null, false],
      [snapshot("0.9.0", "9.0.0"), true],
    ] as const) {
      const view = aboutView(s, lost, "1.0.0", WINDOWS_UA);
      expect(view.facts[2]).toEqual(["WatchDog / MCP 伺服器 版本", watchdogUnknown()]);
      expect(view.facts[4]).toEqual(["WatchDog 的需求", watchdogUnknown()]);
      expect(view.compatibility).toEqual({ kind: "unknown", missing: "watchdog" });
      expect(view.lines).toEqual(["相容性：無法判斷（背景監控失聯）"]);
      expect(compatibilityWarning(view.compatibility)).toBeNull();
    }
    expect(watchdogUnknown()).toBe("無法取得（背景監控失聯）");
  });

  it("holds both sides to 4.2.1, the one version every minimum starts at", () => {
    // Owner decision of 2026-11-19: one version for the whole set; the
    // watchdog's snapshot names 4.2.1 for both of its minimums, and this
    // window's own two are 4.2.1 too.
    expect(MIN_WATCHDOG_VERSION).toBe("4.2.1");
    expect(MIN_USABLE_WATCHDOG_VERSION).toBe("4.2.1");
    const watchdog = (version: string) => snapshot(version, "4.2.1", "4.2.1");
    // An older window with a 4.2.1 watchdog, and a 4.2.1 window with an older watchdog.
    expect(aboutView(watchdog("4.2.1"), false, "4.2.0", WINDOWS_UA).compatibility).toMatchObject({
      kind: "incompatible",
      gui: "unsupported",
      watchdog: null,
    });
    expect(aboutView(watchdog("4.2.0"), false, "4.2.1", WINDOWS_UA).compatibility).toMatchObject({
      kind: "incompatible",
      gui: null,
      watchdog: "unsupported",
    });
    // Equal, or newer on either side: compatible, without a warning.
    for (const [gui, version] of [
      ["4.2.1", "4.2.1"],
      ["4.3.0", "4.2.1"],
      ["4.2.1", "4.3.0"],
    ] as const) {
      const view = aboutView(watchdog(version), false, gui, WINDOWS_UA);
      expect(view.compatibility, `${gui} / ${version}`).toEqual({ kind: "compatible" });
      expect(compatibilityWarning(view.compatibility)).toBeNull();
    }
  });

  it("waits for the window's own version, and says when it cannot be had", () => {
    const early = aboutView(snapshot("4.2.1"), false, null, WINDOWS_UA);
    expect(early.facts[1]).toEqual(["介面版本", "讀取中…"]);
    expect(early.lines).toEqual(["相容性：讀取介面版本中…"]);
    const failed = aboutView(snapshot("4.2.1"), false, null, WINDOWS_UA, { guiFailed: true });
    expect(failed.facts[1]).toEqual(["介面版本", "無法取得"]);
    expect(failed.lines).toEqual(["相容性：無法判斷（取不到介面版本）"]);
    expect(compatibilityWarning(failed.compatibility)).toBeNull();
  });
});

describe("the MCP settings (owner request of 2026-11-05: every platform)", () => {
  it("name the wgal-mcp the installation puts beside the window", () => {
    for (const [platform, command] of Object.entries(MCP_COMMANDS)) {
      expect(JSON.parse(mcpConfig(platform))).toEqual({ mcpServers: { wgal: { command, args: [] } } });
    }
    expect(MCP_COMMANDS.Linux).toBe("/opt/wgal/wgal-mcp");
    expect(MCP_COMMANDS.macOS).toBe("/Applications/WG Always Live.app/Contents/MacOS/wgal-mcp");
    expect(JSON.parse(mcpConfig("Plan 9")).mcpServers.wgal.command).toBe(MCP_COMMANDS.Windows);
  });
});
