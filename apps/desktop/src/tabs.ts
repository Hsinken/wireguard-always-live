// The window's six tabs (WAI-ARIA tabs pattern): one tab stop for the tab
// list, arrow keys, Home and End to move between tabs, and only the selected
// panel shown. The selected tab is remembered per viewer in localStorage,
// which is only a convenience: it can be missing or throw, and the window
// then opens on the tunnels.

import { byId, setAttr } from "./dom";
import { TABS, type TabName, restoreTab } from "./tabname";

const STORAGE_KEY = "wgal.tab";

function remembered(): TabName {
  try {
    return restoreTab(window.localStorage.getItem(STORAGE_KEY));
  } catch {
    return "tunnels";
  }
}

function remember(tab: TabName): void {
  try {
    window.localStorage.setItem(STORAGE_KEY, tab);
  } catch {
    // Not remembered; nothing else depends on it.
  }
}

export class Tabs {
  current: TabName;

  constructor(private readonly onShow: (tab: TabName) => void) {
    this.current = remembered();
    TABS.forEach((name, i) => {
      const tab = byId(`tab-${name}`);
      tab.addEventListener("click", () => this.select(name));
      tab.addEventListener("keydown", (event) => {
        const next =
          event.key === "ArrowRight"
            ? TABS[(i + 1) % TABS.length]
            : event.key === "ArrowLeft"
              ? TABS[(i + TABS.length - 1) % TABS.length]
              : event.key === "Home"
                ? TABS[0]
                : event.key === "End"
                  ? TABS[TABS.length - 1]
                  : undefined;
        if (next !== undefined) {
          event.preventDefault();
          this.select(next, true);
        }
      });
    });
    this.apply();
  }

  select(name: TabName, focus = false): void {
    if (name !== this.current) {
      this.current = name;
      remember(name);
      this.apply();
    }
    if (focus) byId(`tab-${name}`).focus();
  }

  private apply(): void {
    for (const name of TABS) {
      const selected = name === this.current;
      const tab = byId(`tab-${name}`);
      setAttr(tab, "aria-selected", String(selected));
      setAttr(tab, "tabindex", selected ? "0" : "-1");
      byId(`panel-${name}`).hidden = !selected;
    }
    this.onShow(this.current);
  }
}
