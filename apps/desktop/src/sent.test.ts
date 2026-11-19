// FIX-07 (AR-15): a refused form keeps what was typed, an accepted one
// keeps what was typed while it was out, and the MCP tab moves the focus
// only when its confirm row opens or closes.

import { describe, expect, it } from "vitest";

import { confirmFocus, fieldsToClear } from "./sent";

const SENT = { "notify-enabled": "true", "notify-device": "辦公室筆電", "notify-chat": "-1001234567890" };

describe("a sent form", () => {
  it("keeps every field the user changed when the watchdog refuses", () => {
    expect(fieldsToClear(SENT, SENT, false)).toEqual([]);
  });

  it("clears the marks of what was sent once it is accepted", () => {
    expect(fieldsToClear(SENT, SENT, true)).toEqual(["notify-enabled", "notify-device", "notify-chat"]);
  });

  it("keeps what was typed before the answer came", () => {
    const now = { ...SENT, "notify-device": "辦公室筆電 2" };
    expect(fieldsToClear(SENT, now, true)).toEqual(["notify-enabled", "notify-chat"]);
  });

  it("keeps a radio button chosen before the answer came", () => {
    const sent = { "workers-auto": "true", "workers-manual": "false", "workers-count": "4" };
    const now = { "workers-auto": "false", "workers-manual": "true", "workers-count": "4" };
    expect(fieldsToClear(sent, now, true)).toEqual(["workers-count"]);
  });
});

describe("the MCP tab's focus", () => {
  it("goes to the confirm row's no when it opens", () => {
    expect(confirmFocus(false, true, "set_up")).toBe("mcp-client-no");
    expect(confirmFocus(false, true, "remove")).toBe("mcp-client-no");
  });

  it("goes back to the button that opened it when it closes", () => {
    expect(confirmFocus(true, false, "set_up")).toBe("mcp-client-set-up");
    expect(confirmFocus(true, false, "remove")).toBe("mcp-client-remove");
  });

  it("stays where it is when the row was already so (picking a tool, check)", () => {
    expect(confirmFocus(false, false, "set_up")).toBeNull();
    expect(confirmFocus(false, false, "remove")).toBeNull();
    expect(confirmFocus(true, true, "set_up")).toBeNull();
  });
});
