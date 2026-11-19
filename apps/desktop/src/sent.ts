// What a form keeps after it was sent, and where the MCP tab's focus goes
// (FIX-07, AR-15). Pure decisions; main.ts reads and writes the DOM.

/// Each field's value when a form was sent, by element ID: an input's text,
/// or "true"/"false" for a check box or radio button.
export type FieldValues = Readonly<Record<string, string>>;

/// The fields whose "changed by the user" mark may be cleared once the
/// answer is in: none when the watchdog refused (what was typed stays, so
/// it is not overwritten by the old values), and when it accepted only
/// those still holding what was sent: anything typed while the request was
/// out is kept.
export function fieldsToClear(sent: FieldValues, now: FieldValues, accepted: boolean): string[] {
  if (!accepted) return [];
  return Object.keys(sent).filter((id) => now[id] === sent[id]);
}

/// Where the focus goes when the MCP tab's confirm row is asked to be open
/// (`open`) or closed: to its "no" button when it opens, back to the button
/// that opened it when it closes, and nowhere when it was already so, as
/// when another tool is picked or "check" is pressed.
export function confirmFocus(
  wasOpen: boolean,
  open: boolean,
  mode: "set_up" | "remove",
): "mcp-client-no" | "mcp-client-set-up" | "mcp-client-remove" | null {
  if (wasOpen === open) return null;
  if (open) return "mcp-client-no";
  return mode === "remove" ? "mcp-client-remove" : "mcp-client-set-up";
}
