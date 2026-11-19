// The window's refresh interval: stored per viewer, validated, and the
// lost-link threshold that follows it, on the window's side (the shell's
// side is tested in src-tauri/src/lib.rs); and the window's language.

import { describe, expect, it } from "vitest";

import { LANGUAGES, SYSTEM_CHOICE } from "./i18n";
import {
  LOST_FLOOR_MS,
  REFRESH_SECS,
  loadLanguage,
  loadRefreshSecs,
  lostAfterMs,
  parseRefreshSecs,
  saveLanguage,
  saveRefreshSecs,
} from "./prefs";
import type { Snapshot } from "./protocol";
import { LOST_AFTER_MS, countdown, isLost, viewModel, type Link } from "./view";

class MemoryStorage {
  readonly items = new Map<string, string>();
  getItem(key: string): string | null {
    return this.items.get(key) ?? null;
  }
  setItem(key: string, value: string): void {
    this.items.set(key, value);
  }
}

const broken = {
  getItem(): string | null {
    throw new Error("SecurityError");
  },
  setItem(): void {
    throw new Error("QuotaExceededError");
  },
};

describe("the refresh interval", () => {
  it("accepts whole seconds from 3 to 300", () => {
    expect(REFRESH_SECS).toEqual({ min: 3, max: 300, default: 5 });
    for (const [text, value] of [["3", 3], ["5", 5], [" 60 ", 60], ["300", 300]] as const) {
      expect(parseRefreshSecs(text), text).toBe(value);
    }
    for (const bad of [null, "", "2", "301", "0", "-5", "5.5", "1e2", "abc", "0300"]) {
      expect(parseRefreshSecs(bad), String(bad)).toBeNull();
    }
  });

  it("is remembered per viewer and falls back to 5 s", () => {
    const storage = new MemoryStorage();
    expect(loadRefreshSecs(storage)).toBe(5);
    expect(saveRefreshSecs(30, storage)).toBe(true);
    expect(loadRefreshSecs(storage)).toBe(30);
    expect([...storage.items.keys()]).toEqual(["wgal.refreshSecs"]);
    // A stored value out of range or garbled is the default again.
    storage.setItem("wgal.refreshSecs", "9999");
    expect(loadRefreshSecs(storage)).toBe(5);
    storage.setItem("wgal.refreshSecs", "fast");
    expect(loadRefreshSecs(storage)).toBe(5);
  });

  it("works without storage", () => {
    expect(loadRefreshSecs(broken)).toBe(5);
    expect(saveRefreshSecs(30, broken)).toBe(false);
    expect(loadRefreshSecs(null)).toBe(5);
    expect(saveRefreshSecs(30, null)).toBe(false);
  });
});

describe("the lost-link threshold follows the interval", () => {
  it("is max(15 s, 3 intervals)", () => {
    expect(LOST_FLOOR_MS).toBe(LOST_AFTER_MS);
    expect(lostAfterMs(3)).toBe(15_000);
    expect(lostAfterMs(5)).toBe(15_000);
    expect(lostAfterMs(6)).toBe(18_000);
    expect(lostAfterMs(60)).toBe(180_000);
    expect(lostAfterMs(300)).toBe(900_000);
  });

  it("does not show 失聯 between two slow refreshes, and does after three", () => {
    const snapshot = { generated_at_unix_ms: 0 } as unknown as Snapshot;
    const slow: Link = { snapshot, receivedAtMono: 0, lastError: null, lostAfterMs: lostAfterMs(60) };
    expect(isLost(slow, 61_000)).toBe(false);
    expect(isLost(slow, 180_000)).toBe(false);
    expect(isLost(slow, 180_001)).toBe(true);
    // Without a threshold, the plan's 15 s.
    expect(isLost({ snapshot, receivedAtMono: 0, lastError: null }, 15_001)).toBe(true);
    // An error is lost at once, whatever the interval.
    expect(isLost({ ...slow, lastError: () => ({ text: "gone", detail: null }) }, 1)).toBe(true);
  });

  it("names the threshold in the overlay", () => {
    const link: Link = { snapshot: null, receivedAtMono: null, lastError: null, lostAfterMs: lostAfterMs(60) };
    expect(viewModel(link, 0).overlay?.text).toContain("超過 180 秒沒有收到");
  });

  it("keeps countdowns right between slow refreshes", () => {
    const T = 1_767_225_600_000;
    // A retry 5 minutes after the snapshot, 200 s later with no new one.
    expect(countdown(T + 300_000, T, 200_000)).toBe("剩 1 分 40 秒");
    expect(countdown(T + 300_000, T, 299_000)).toBe("剩 1 秒");
    expect(countdown(T + 300_000, T, 300_000)).toBe("等待更新");
  });
});

describe("the language", () => {
  it("is stored under its own key and read back only when a file names it or it follows the system", () => {
    const storage = new MemoryStorage();
    expect(loadLanguage(storage)).toBe(SYSTEM_CHOICE);
    expect(saveLanguage(SYSTEM_CHOICE, storage)).toBe(true);
    expect(loadLanguage(storage)).toBe(SYSTEM_CHOICE);
    for (const l of LANGUAGES) {
      expect(saveLanguage(l.tag, storage)).toBe(true);
      expect(storage.items.get("wgal.language")).toBe(l.tag);
      expect(loadLanguage(storage)).toBe(l.tag);
    }
    for (const bad of ["xx-YY", "", "zh-tw", "../zh-TW", "<script>"]) {
      storage.setItem("wgal.language", bad);
      expect(loadLanguage(storage), bad).toBe(SYSTEM_CHOICE);
    }
  });

  it("follows the system without storage, and says when it could not be stored", () => {
    expect(loadLanguage(null)).toBe(SYSTEM_CHOICE);
    expect(loadLanguage(broken)).toBe(SYSTEM_CHOICE);
    expect(saveLanguage("zh-TW", null)).toBe(false);
    expect(saveLanguage("zh-TW", broken)).toBe(false);
  });
});
