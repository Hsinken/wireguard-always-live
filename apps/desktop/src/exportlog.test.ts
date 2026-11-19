// The shell's answers to a save: every reason the shell can give
// (`src-tauri/src/export.rs` `Reason`) is one the window reads
// (`exportlog.ts` `REASONS`) and has words for. `exportlog.ts` uses the
// DOM, which tests are not typed with, so both lists are read from source.

import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import { type Key, t } from "./i18n";

const source = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");

/// The shell's reasons as it sends them: its `Reason` variants in
/// snake_case.
function shellReasons(): string[] {
  const body = /pub\(crate\) enum Reason \{([^}]*)\}/.exec(source("../src-tauri/src/export.rs"))?.[1] ?? "";
  return [...body.matchAll(/^\s*([A-Z][A-Za-z]*),/gm)].map((m) =>
    m[1]!.replace(/[A-Z]/g, (c, i: number) => (i === 0 ? c.toLowerCase() : `_${c.toLowerCase()}`)),
  );
}

/// The reasons the window reads.
function windowReasons(): string[] {
  const body = /const REASONS[^[]*\[([^\]]*)\]/.exec(source("./exportlog.ts"))?.[1] ?? "";
  return [...body.matchAll(/"([a-z_]+)"/g)].map((m) => m[1]!);
}

describe("the shell's refusals", () => {
  it("are all read by the window, each with its own words", () => {
    const shell = shellReasons();
    expect(shell).toContain("linked_folder");
    expect(shell).toContain("redirected");
    expect(windowReasons().sort()).toEqual([...shell].sort());
    for (const reason of shell) {
      const key = `export.refused_${reason}` as Key;
      expect(t(key), reason).not.toBe(key);
    }
  });

  it("say why a linked or redirected folder got nothing", () => {
    expect(t("export.refused_linked_folder")).toBe("匯出資料夾是符號連結或交接點，為了安全沒有寫入");
    expect(t("export.refused_redirected")).toBe(
      "新檔案被導向匯出資料夾以外的位置，已停止匯出：沒有寫入任何內容，為了安全也沒有刪除那個空白檔案",
    );
  });

  it("say that a file not written whole is left, never removed (Codex re-review P2-N1)", () => {
    expect(t("export.refused_cannot_write")).toBe("無法寫入檔案；未完成的檔案沒有刪除，留在原處，可以自行刪除");
  });
});
