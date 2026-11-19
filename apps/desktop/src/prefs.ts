// The window's own preferences, kept per viewer in localStorage (owner's
// requests of 2026-11-03 and 2026-11-05): how often the window asks the
// watchdog for its state, and which language it speaks (by default the
// system's). Not watchdog settings and not in the contract. Storage is
// only a convenience: every read and write may throw, and a missing or
// bad value means the default.

import { LANGUAGES, SYSTEM_CHOICE } from "./i18n";

/// The refresh interval in seconds: 3 to 300, 5 by default (plan §6.2's
/// 5 s heartbeat, which the owner made adjustable).
export const REFRESH_SECS = { min: 3, max: 300, default: 5 } as const;

const STORAGE_KEY = "wgal.refreshSecs";

/// The part of `localStorage` used here.
export interface KeyValueStore {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/// How long the window waits without a snapshot before it shows the
/// watchdog as lost: 15 s (plan §6.2), or three intervals when that is
/// longer, so a long interval does not show the watchdog as lost between
/// two refreshes.
export const LOST_FLOOR_MS = 15_000;

export function lostAfterMs(refreshSecs: number): number {
  return Math.max(LOST_FLOOR_MS, 3 * refreshSecs * 1000);
}

/// A stored or typed interval, or null when it is not a whole number of
/// seconds in range.
export function parseRefreshSecs(text: string | null): number | null {
  if (text === null) return null;
  const value = text.trim();
  if (!/^[0-9]{1,3}$/.test(value)) return null;
  const n = Number(value);
  return n >= REFRESH_SECS.min && n <= REFRESH_SECS.max ? n : null;
}

/// The stored interval, or the default.
export function loadRefreshSecs(storage: Pick<KeyValueStore, "getItem"> | null = safeStorage()): number {
  try {
    return parseRefreshSecs(storage?.getItem(STORAGE_KEY) ?? null) ?? REFRESH_SECS.default;
  } catch {
    return REFRESH_SECS.default;
  }
}

/// Stores the interval; false when it could not be stored (it still applies
/// to this window until it closes).
export function saveRefreshSecs(secs: number, storage: Pick<KeyValueStore, "setItem"> | null = safeStorage()): boolean {
  try {
    if (storage === null) return false;
    storage.setItem(STORAGE_KEY, String(secs));
    return true;
  } catch {
    return false;
  }
}

const LANGUAGE_KEY = "wgal.language";

/// The stored language choice: a language's tag when there is a language
/// file for it, otherwise (nothing stored, the system choice, or a tag
/// with no file) the system choice.
export function loadLanguage(storage: Pick<KeyValueStore, "getItem"> | null = safeStorage()): string {
  try {
    const stored = storage?.getItem(LANGUAGE_KEY) ?? null;
    return LANGUAGES.some((l) => l.tag === stored) ? (stored as string) : SYSTEM_CHOICE;
  } catch {
    return SYSTEM_CHOICE;
  }
}

/// Stores the language choice (a tag or the system choice); false when it
/// could not be stored (it still applies to this window until it closes).
export function saveLanguage(choice: string, storage: Pick<KeyValueStore, "setItem"> | null = safeStorage()): boolean {
  try {
    if (storage === null) return false;
    storage.setItem(LANGUAGE_KEY, choice);
    return true;
  } catch {
    return false;
  }
}

/// The viewer's localStorage, or null where there is none or it throws.
export function safeStorage(): KeyValueStore | null {
  try {
    return (globalThis as { localStorage?: KeyValueStore }).localStorage ?? null;
  } catch {
    return null;
  }
}
