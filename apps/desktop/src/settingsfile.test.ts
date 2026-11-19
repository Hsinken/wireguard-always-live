// The settings file (owner decision of 2026-11-04): what this window reads
// as one, and how it words what an import did or would do.

import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import type { ImportResult } from "./protocol";
import { MAX_SETTINGS_FILE_BYTES, autoRecoveryText, importLines, importOutcomeText, readSettingsFile } from "./settingsfile";

const exported = readFileSync(
  new URL("../../../crates/wgal-protocol/fixtures/response_settings_export.json", import.meta.url),
  "utf8",
);
const document = (JSON.parse(exported) as { outcome: { ok: { document: unknown } } }).outcome.ok.document;

describe("reading a settings file", () => {
  it("takes the watchdog's own export", () => {
    const read = readSettingsFile(JSON.stringify(document));
    expect(read.ok).toBe(true);
    if (read.ok) expect(read.document.tunnels).toHaveLength(2);
  });

  it("says why anything else is not one", () => {
    const error = (text: string) => {
      const read = readSettingsFile(text);
      return read.ok ? null : read.error;
    };
    expect(error("not json")).toBe("這不是 JSON 檔，不是 WGAL 的設定檔");
    expect(error("[]")).toBe("這不是 WGAL 的設定檔");
    expect(error(JSON.stringify({ ...(document as object), format: "other" }))).toBe("這不是 WGAL 的設定檔");
    expect(error(JSON.stringify({ ...(document as object), version: 2 }))).toBe(
      "這是第 2 版的設定檔，這個 WGAL 只讀得懂第 1 版",
    );
    expect(error(JSON.stringify({ ...(document as object), tunnels: {} }))).toBe("這不是 WGAL 的設定檔");
    expect(error("x".repeat(MAX_SETTINGS_FILE_BYTES + 1))).toBe("檔案太大，不是 WGAL 的設定檔");
  });
});

describe("what an import did", () => {
  it("words every outcome, as done and as it would be", () => {
    expect(importOutcomeText({ kind: "applied" }, true)).toBe("會套用");
    expect(importOutcomeText({ kind: "applied" }, false)).toBe("已套用");
    expect(importOutcomeText({ kind: "registered" }, true)).toBe("會登記並套用");
    expect(importOutcomeText({ kind: "not_found" }, false)).toBe("這台電腦沒有這條隧道，無法恢復");
    expect(importOutcomeText({ kind: "ambiguous" }, false)).toContain("可改用 ID 比對");
    expect(importOutcomeText({ kind: "duplicate_in_file" }, false)).toBe("檔案裡有重複的項目，無法恢復");
    expect(importOutcomeText({ kind: "not_registered" }, false)).toBe("尚未登記，沒有套用");
    expect(importOutcomeText({ kind: "unsupported" }, false)).toBe("官方程式不允許 WGAL 控制，無法恢復");
    const refused = importOutcomeText({ kind: "refused", code: "unauthorized", reason: "needs_administrator_or_listed_tunnel" }, false);
    expect(refused.startsWith("背景監控拒絕：")).toBe(true);
    expect(refused.length).toBeGreaterThan("背景監控拒絕：".length);
    expect(autoRecoveryText({ kind: "waiting_for_baseline" })).toContain("10 分鐘內");
    expect(autoRecoveryText({ kind: "on" })).toBe("自動復原開啟");
    expect(autoRecoveryText({ kind: "refused", code: "invalid_state", reason: null }).startsWith("自動復原未開啟：")).toBe(true);
  });

  it("lists the host settings and the notification switch first, then each entry by its name", () => {
    const result: ImportResult = {
      dry_run: true,
      host: { kind: "applied" },
      notifications: { kind: "refused", code: "storage_unavailable", reason: "storage_save_failed" },
      tunnels: [
        { index: 0, name: "Office", tunnel_id: "office-1", outcome: { kind: "registered" }, auto_recovery: { kind: "off" } },
      ],
    };
    const lines = importLines(result);
    expect(lines[0]).toBe("主機設定（工作執行緒、時區）：會套用");
    expect(lines[1]?.startsWith("通知開關：")).toBe(true);
    expect(lines[1]).not.toContain("會套用");
    expect(lines[2]).toBe("Office：會登記並套用；自動復原關閉");
    // A watchdog that did not report the switch apart: no line for it.
    expect(importLines({ ...result, notifications: null })).toEqual([
      "主機設定（工作執行緒、時區）：會套用",
      "Office：會登記並套用；自動復原關閉",
    ]);
  });
});
