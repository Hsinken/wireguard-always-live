// Versions (AGENTS.md, "Versions"). Since 4.2.1 the window, the watchdog,
// wgal-mcp and wgal share one version (owner decision of 2026-11-19): the
// Cargo workspace's, written nowhere else. The wgal-desktop crate inherits it
// (its `app_version()` is `CARGO_PKG_VERSION`), the Tauri configuration has
// none (Tauri then takes the crate's) and neither has this package. Every
// minimum each side states for the other is at most that version:
// `MIN_WATCHDOG_VERSION` here, `MIN_GUI_VERSION` in Rust (`limits.json`).

import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import {
  MIN_GUI_VERSION,
  MIN_USABLE_GUI_VERSION,
  MIN_USABLE_WATCHDOG_VERSION,
  MIN_WATCHDOG_VERSION,
  versionAtLeast,
} from "./index";

const read = (path: string): string => readFileSync(new URL(path, import.meta.url), "utf8");

const VERSION = /^\d+\.\d+\.\d+([-+][0-9A-Za-z.+-]+)?$/;

/** The body of a TOML section, up to the next section. */
function tomlSection(text: string, section: string): string {
  const header = `[${section}]`;
  const at = text.indexOf(header);
  if (at < 0) throw new Error(`no [${section}]`);
  return text.slice(at + header.length).split(/^\[/m)[0] ?? "";
}

/** `version = "…"` of a TOML section. */
function tomlVersion(text: string, section: string): string {
  const match = /^version = "([^"]+)"$/m.exec(tomlSection(text, section));
  if (!match?.[1]) throw new Error(`no version in [${section}]`);
  return match[1];
}

const version = tomlVersion(read("../../../../Cargo.toml"), "workspace.package");

describe("versions", () => {
  it("are written in one place, the workspace, which the window inherits", () => {
    expect(version).toMatch(VERSION);
    const crate = tomlSection(read("../../src-tauri/Cargo.toml"), "package");
    expect(crate).toMatch(/^version\.workspace = true$/m);
    expect(crate).not.toMatch(/^version = /m);
    const tauri = JSON.parse(read("../../src-tauri/tauri.conf.json")) as Record<string, unknown>;
    expect(tauri).not.toHaveProperty("version");
    const pkg = JSON.parse(read("../../package.json")) as Record<string, unknown>;
    expect(pkg).not.toHaveProperty("version");
  });

  it("put every minimum at or below the one version", () => {
    const limits = JSON.parse(read("../../../../crates/wgal-protocol/fixtures/limits.json")) as {
      min_gui_version: string;
      min_usable_gui_version: string;
    };
    expect(limits.min_gui_version).toBe(MIN_GUI_VERSION);
    expect(limits.min_usable_gui_version).toBe(MIN_USABLE_GUI_VERSION);
    for (const minimum of [MIN_GUI_VERSION, MIN_USABLE_GUI_VERSION, MIN_WATCHDOG_VERSION, MIN_USABLE_WATCHDOG_VERSION]) {
      expect(minimum).toMatch(VERSION);
      expect(versionAtLeast(version, minimum), `${version} >= ${minimum}`).toBe(true);
    }
  });

  it("have a usable minimum no newer than the full one", () => {
    expect(versionAtLeast(MIN_GUI_VERSION, MIN_USABLE_GUI_VERSION)).toBe(true);
    expect(versionAtLeast(MIN_WATCHDOG_VERSION, MIN_USABLE_WATCHDOG_VERSION)).toBe(true);
  });

  it("compare by their numbers, as Rust does", () => {
    for (const [have, need] of [
      ["1.0.0", "1.0.0"],
      ["1.0.1", "1.0.0"],
      ["1.10.0", "1.9.3"],
      ["2.0.0", "1.99.99"],
      ["1.0", "1.0.0"],
      ["1.0.0-rc.1", "1.0.0"],
      ["1.2.0+build.7", "1.2"],
    ] as const) {
      expect(versionAtLeast(have, need), `${have} >= ${need}`).toBe(true);
    }
    for (const [have, need] of [
      ["0.9.9", "1.0.0"],
      ["1.9.3", "1.10.0"],
      ["1.0.0", "1.0.1"],
    ] as const) {
      expect(versionAtLeast(have, need), `${have} < ${need}`).toBe(false);
    }
  });
});
