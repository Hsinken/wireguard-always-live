// One-click MCP client setup without the DOM (owner request of 2026-11-12).

import { describe, expect, it } from "vitest";

import {
  CLIENT_IDS,
  CLIENT_NAMES,
  CLIENT_STATES,
  NOT_SET_UP,
  asClientView,
  asClientViews,
  asNotSetUp,
  checkedText,
  questionText,
  reasonText,
  removeQuestionText,
  stateText,
} from "./mcpclients";

const view = {
  client: "cursor",
  path: String.raw`C:\Users\you\.cursor\mcp.json`,
  state: "not_configured",
  preview: '{\n  "mcpServers": {}\n}',
};

describe("the shell's views", () => {
  it("are taken only in the shape the shell sends", () => {
    expect(asClientView(view)).toEqual(view);
    expect(asClientView({ ...view, client: "notepad" })).toBeNull();
    expect(asClientView({ ...view, state: "maybe" })).toBeNull();
    expect(asClientView({ ...view, path: 1 })).toBeNull();
    expect(asClientView({ ...view, preview: "x".repeat(5000) })).toBeNull();
    expect(asClientView(null)).toBeNull();
  });

  it("are a list of tools, each at most once", () => {
    expect(asClientViews([view])).toEqual([view]);
    expect(asClientViews([])).toEqual([]);
    expect(asClientViews([view, view])).toBeNull();
    expect(asClientViews([view, { ...view, client: "x" }])).toBeNull();
    expect(asClientViews({})).toBeNull();
  });

  it("name every tool, in the shell's order", () => {
    expect(CLIENT_IDS).toEqual([
      "claude_code",
      "claude_desktop",
      "cursor",
      "vs_code",
      "trae",
      "kiro",
      "codex",
      "antigravity",
      "gemini_cli",
    ]);
    for (const id of CLIENT_IDS) expect(CLIENT_NAMES[id]).not.toBe("");
  });
});

describe("the words", () => {
  it("say each state with the file", () => {
    for (const state of CLIENT_STATES) {
      const text = stateText({ ...view, state } as Parameters<typeof stateText>[0]);
      expect(text).toContain(view.path);
      expect(text).not.toContain("{");
    }
    expect(stateText({ ...view, state: "configured" } as Parameters<typeof stateText>[0])).toBe(`已設定：${view.path}`);
  });

  it("say each reason with the tool, and take anything else as a failed write", () => {
    for (const reason of NOT_SET_UP) {
      const text = reasonText(reason, "kiro", view.path);
      expect(text).not.toContain("{");
      if (reason !== "no_server") expect(text).toContain("Kiro");
    }
    expect(reasonText("damaged", "kiro", view.path)).toContain(view.path);
    expect(asNotSetUp({ reason: "changed" })).toBe("changed");
    expect(asNotSetUp({ reason: "rm -rf" })).toBe("io");
    expect(asNotSetUp("boom")).toBe("io");
  });

  it("ask before removing, and say what a check found", () => {
    const question = removeQuestionText(view as Parameters<typeof removeQuestionText>[0]);
    expect(question).toContain(view.path);
    expect(question).toContain("請先結束 Cursor");
    const configured = { ...view, state: "configured" } as Parameters<typeof checkedText>[0];
    expect(checkedText(configured)).toBe(`Cursor：已設定：${view.path}`);
    expect(reasonText("not_there", "cursor", view.path)).toContain("沒有 wgal");
  });

  it("ask before writing, and ask Claude Code users to quit it first", () => {
    const question = questionText(view as Parameters<typeof questionText>[0]);
    expect(question).toContain(view.path);
    expect(question).toContain("請先結束 Cursor");
    expect(question).not.toContain("Claude Code");
    const claude = questionText({ ...view, client: "claude_code" } as Parameters<typeof questionText>[0]);
    expect(claude).toContain("每一個 Claude Code 都要結束");
  });
});
