// The window's tabs by name, and which one opens first. No DOM here, so it
// is tested without a browser; `tabs.ts` does the rest.

export const TABS = ["tunnels", "log", "notify", "settings", "mcp", "about"] as const;
export type TabName = (typeof TABS)[number];

/// The remembered tab, or the tunnels when nothing usable is remembered.
export function restoreTab(stored: string | null): TabName {
  return (TABS as readonly string[]).includes(stored ?? "") ? (stored as TabName) : "tunnels";
}
