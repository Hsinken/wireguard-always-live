// The settings tab's settings-file group (owner decision of 2026-11-04):
// the buttons, the file the user picked, the two import choices, what a dry
// run says and the import itself. The words come from `settingsfile.ts`.

import { byId, el, setControl, setHidden, setText, speak, unavailable } from "./dom";
import type { SavedExport } from "./exportlog";
import { exportFailure } from "./exportlog";
import { t } from "./i18n";
import type { ImportMatch, Outcome, Request, SettingsDocument, Snapshot } from "./protocol";
import { type Said, type Words, plain } from "./said";
import { MAX_SETTINGS_FILE_BYTES, importLines, readSettingsFile } from "./settingsfile";

/// Saves a settings document under the device's name.
export type SaveSettings = (
  document: SettingsDocument,
  deviceName: string,
  utcOffsetMinutes: number,
) => Promise<SavedExport>;

export interface SettingsFileContext {
  ask(request: Request): Promise<Outcome>;
  refusal(request: Request, outcome: Outcome): Said;
  noAnswer(error: unknown): Said;
  snapshot(): Snapshot | null;
  /// Export and import are offered now: the watchdog answers and has them.
  offered(): boolean;
  /// The configured zone's offset now, in minutes, for the file's name.
  offsetMinutes(): number;
  save: SaveSettings;
}

/// The settings tab's settings-file group.
export class SettingsFileGroup {
  private readonly exportButton = byId("settings-export") as HTMLButtonElement;
  private readonly importButton = byId("settings-import") as HTMLButtonElement;
  private readonly file = byId("settings-file") as HTMLInputElement;
  private readonly status = byId("settings-file-status");
  private readonly panel = byId("import-panel");
  private readonly about = byId("import-about");
  private readonly match = byId("import-match") as HTMLSelectElement;
  private readonly register = byId("import-register") as HTMLInputElement;
  private readonly monitor = byId("import-monitor") as HTMLInputElement;
  private readonly apply = byId("import-apply") as HTMLButtonElement;
  private readonly cancel = byId("import-cancel") as HTMLButtonElement;
  private readonly results = byId("import-results");
  private document: SettingsDocument | null = null;
  /// Counts previews, so an answer to an older one is not shown.
  private generation = 0;
  private busy = false;

  constructor(private readonly ctx: SettingsFileContext) {
    this.exportButton.addEventListener("click", () => void this.export());
    this.importButton.addEventListener("click", () => {
      if (!unavailable(this.importButton)) this.file.click();
    });
    this.file.addEventListener("change", () => void this.picked());
    for (const control of [this.match, this.register, this.monitor]) {
      control.addEventListener("change", () => void this.preview());
    }
    this.apply.addEventListener("click", () => {
      if (!unavailable(this.apply)) void this.run(false);
    });
    this.cancel.addEventListener("click", () => this.close());
    this.render();
  }

  /// The heartbeat: whether the buttons may be used now.
  render(): void {
    const offered = this.ctx.offered();
    const why = offered ? null : t("settings_file.not_offered");
    for (const button of [this.exportButton, this.importButton]) {
      setControl(button, why === null && !this.busy ? { kind: "enabled" } : { kind: "disabled", why: why ?? t("settings_file.busy") });
    }
    setControl(
      this.apply,
      offered && !this.busy && this.document !== null ? { kind: "enabled" } : { kind: "disabled", why: why ?? t("settings_file.busy") },
    );
  }

  private say(words: Words | null): void {
    speak(this.status, words);
  }

  private async export(): Promise<void> {
    if (unavailable(this.exportButton)) return;
    this.busy = true;
    this.render();
    this.say(() => plain(t("settings_file.exporting")));
    const request: Request = { op: "export_settings" };
    try {
      const outcome = await this.ctx.ask(request);
      if ("ok" in outcome && outcome.ok.kind === "settings_export") {
        const device = this.ctx.snapshot()?.notification.device_name ?? "";
        const saved = await this.ctx.save(outcome.ok.document, device, this.ctx.offsetMinutes());
        this.say(() =>
          plain(
            t("settings_file.exported", { folder: t(`export.folder_${saved.folder}`), name: saved.name }),
          ),
        );
      } else {
        this.say(() => this.ctx.refusal(request, outcome));
      }
    } catch (error) {
      this.say(() => {
        const failure = exportFailure(error);
        return { text: t("settings_file.export_failed", { error: failure.text }), detail: failure.detail };
      });
    } finally {
      this.busy = false;
      this.render();
    }
  }

  private async picked(): Promise<void> {
    const chosen = this.file.files?.[0];
    this.file.value = "";
    if (chosen === undefined) return;
    if (chosen.size > MAX_SETTINGS_FILE_BYTES) {
      this.say(() => plain(t("settings_file.too_large")));
      return;
    }
    const read = readSettingsFile(await chosen.text());
    if (!read.ok) {
      this.say(() => plain(read.error));
      return;
    }
    this.document = read.document;
    setText(
      this.about,
      t("settings_file.about", {
        name: chosen.name,
        count: read.document.tunnels.length,
        version: read.document.watchdog_version,
      }),
    );
    // The owner's defaults: register missing tunnels, leave new ones
    // unmonitored.
    this.match.value = "name";
    this.register.checked = true;
    this.monitor.checked = false;
    setHidden(this.panel, false);
    this.say(null);
    await this.preview();
  }

  private close(): void {
    this.document = null;
    this.generation += 1;
    setHidden(this.panel, true);
    this.results.replaceChildren();
    this.say(null);
    this.render();
  }

  private preview(): Promise<void> {
    return this.run(true);
  }

  /// A dry run to show what an import would do, or the import itself.
  private async run(dryRun: boolean): Promise<void> {
    const document = this.document;
    if (document === null) return;
    const generation = ++this.generation;
    if (!dryRun) {
      this.busy = true;
      this.render();
    }
    this.say(() => plain(t(dryRun ? "settings_file.previewing" : "settings_file.importing")));
    const request: Request = {
      op: "import_settings",
      document,
      options: {
        match_by: this.match.value as ImportMatch,
        register_missing: this.register.checked,
        monitor_new: this.monitor.checked,
        dry_run: dryRun,
      },
    };
    try {
      const outcome = await this.ctx.ask(request);
      if (generation !== this.generation) return;
      if ("ok" in outcome && outcome.ok.kind === "settings_import") {
        const lines = importLines(outcome.ok.result);
        this.results.replaceChildren(...lines.map((line) => el("li", { text: line })));
        this.say(() => plain(t(dryRun ? "settings_file.previewed" : "settings_file.imported")));
        if (!dryRun) this.document = null;
      } else {
        this.say(() => this.ctx.refusal(request, outcome));
      }
    } catch (error) {
      if (generation === this.generation) this.say(() => this.ctx.noAnswer(error));
    } finally {
      if (!dryRun) {
        this.busy = false;
        this.render();
      }
    }
  }
}
