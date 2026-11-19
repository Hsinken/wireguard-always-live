// The log tab: the watchdog's event log newest first, read a page at a time
// with `ReadEventLog`, filtered by tunnel, refreshed while the tab is open.
// Lines are kept per key (see `keyed.ts`), so a refresh adds the new ones
// on top without rebuilding the rest or moving the scroll position. The
// status line is a live region, so the background refresh leaves it alone:
// only the first load, a refresh the user asked for, and errors change it.
// The watchdog-link filter (`log.filter_watchdog`) shows the watchdog's own
// start and stop lines merged with what this window saw of its link to it
// (`watchlog.ts`), which it can show even while the watchdog cannot be read.

import { byId, el, setAttr, setHidden, setText, showSaid } from "./dom";
import { type Key, language, t } from "./i18n";
import { KeyedList } from "./keyed";
import {
  type LogFilter,
  type LogState,
  NO_LOG,
  SYSTEM_OPTION,
  WATCHDOG_OPTION,
  entryView,
  logFilterOf,
  logFilterValue,
  olderThan,
  systemOffer,
  withNewest,
  withOlder,
} from "./log";
import type { EventEntry, Outcome, Request, TunnelId } from "./protocol";
import { type Said, type Words, plain } from "./said";
import type { Zone } from "./time";
import {
  type LinkRecord,
  type LogLineView,
  guiLabel,
  watchdogLabel,
  watchdogLineView,
  watchdogLines,
} from "./watchlog";

export interface LogContext {
  /// Sends a request; throws when the watchdog does not answer.
  ask(request: Request): Promise<Outcome>;
  /// A refusal in words, and what is said when the watchdog could not be
  /// asked at all.
  refusal(request: Request, outcome: Outcome): Said;
  noAnswer(error: unknown): Said;
  /// Registered tunnels, in the snapshot's order, with their card names.
  tunnels(): ReadonlyArray<readonly [TunnelId, string]>;
  zone(): Zone | null;
  /// The time whose year log times leave out: the snapshot's.
  refMs(): number;
  /// The window's own records of its link to the watchdog, newest first,
  /// and whether they are kept beyond this window.
  linkRecords(): readonly LinkRecord[];
  linkStored(): boolean;
  /// The watchdog's version in the latest snapshot, null before one and
  /// while the watchdog is lost (it may come back as another version): the
  /// lines about no tunnel are offered only by a watchdog that reads them
  /// apart (`systemFilterOffered`).
  watchdogVersion(): string | null;
}

interface LogRow {
  readonly node: HTMLLIElement;
  readonly time: HTMLTimeElement;
  readonly category: HTMLSpanElement;
  readonly who: HTMLSpanElement;
  readonly text: HTMLSpanElement;
}

function createRow(): LogRow {
  const row: LogRow = {
    node: el("li", { className: "entry" }),
    time: el("time"),
    category: el("span", { className: "cat" }),
    who: el("span", { className: "who" }),
    text: el("span", { className: "what" }),
  };
  row.node.append(row.time, row.category, row.who, row.text);
  return row;
}

function updateRow(row: LogRow, view: LogLineView): void {
  setAttr(row.node, "class", `entry cat-${view.category}${view.tone === undefined ? "" : ` tone-${view.tone}`}`);
  setText(row.time, view.time);
  setText(row.category, `${view.icon} ${view.categoryLabel}`);
  setText(row.who, view.who);
  setText(row.text, view.text);
}

export class LogTab {
  private state: LogState = NO_LOG;
  private filter: LogFilter = { kind: "all" };
  /// Counts filter changes: an answer for an older filter is dropped.
  private generation = 0;
  private loadingNewest = false;
  /// The newest page being read was asked for by the user.
  private manual = false;
  /// Said after a refresh the user asked for, until the next one.
  private note: Key | null = null;
  private loadingOlder = false;
  /// Why the last read failed, worded in the language of the moment.
  private error: Words | null = null;
  private active = false;
  private optionsKey = "";
  /// The watchdog version that refused the system lines as a malformed
  /// request (an older one than its snapshot said, during a downgrade): not
  /// offered again while the snapshot names that version.
  private systemRefusedBy: string | null = null;
  private readonly rows: KeyedList<string, LogLineView, Node, LogRow>;
  private readonly select = byId("log-filter") as HTMLSelectElement;
  private readonly more = byId("log-more") as HTMLButtonElement;

  constructor(private readonly ctx: LogContext) {
    this.rows = new KeyedList<string, LogLineView, Node, LogRow>(byId("log"), createRow, updateRow);
    this.select.addEventListener("change", () => {
      this.filter = logFilterOf(this.select.value);
      this.restart();
    });
    byId("log-refresh").addEventListener("click", () => void this.refresh(true));
    this.more.addEventListener("click", () => void this.loadOlder());
  }

  /// The tab was shown or hidden. Showing it reads the newest lines.
  show(active: boolean): void {
    this.active = active;
    if (active) void this.refresh(false);
  }

  /// The heartbeat: refreshes while the tab is open and the window visible.
  tick(): void {
    if (this.active && document.visibilityState === "visible") void this.refresh(false);
  }

  private restart(): void {
    this.generation += 1;
    this.state = NO_LOG;
    this.error = null;
    this.loadingNewest = false;
    this.manual = false;
    this.note = null;
    this.loadingOlder = false;
    this.render();
    void this.refresh(false);
  }

  private async read(beforeSeq: number | null): Promise<{ entries: EventEntry[]; more: boolean } | null> {
    const request: Request = { op: "read_event_log", filter: this.filter, before_seq: beforeSeq };
    const generation = this.generation;
    // The version this request goes to, for a refusal (Codex P3-N16).
    const sentTo = this.ctx.watchdogVersion();
    let answer: { entries: EventEntry[]; more: boolean } | null = null;
    let error: Words | null = null;
    try {
      const outcome = await this.ctx.ask(request);
      if ("ok" in outcome && outcome.ok.kind === "event_log") {
        answer = { entries: outcome.ok.entries, more: outcome.ok.more };
      } else if (
        request.filter.kind === "system" &&
        "err" in outcome &&
        outcome.err.code === "malformed_request" &&
        generation === this.generation
      ) {
        // A watchdog that does not read the system lines apart after all:
        // back to every line, and not offered again for the version it was
        // sent to (`systemOffer`).
        this.systemRefusedBy = sentTo ?? "";
        this.optionsKey = "";
        return null;
      } else {
        error = () => this.ctx.refusal(request, outcome);
      }
    } catch (e) {
      error = () => this.ctx.noAnswer(e);
    }
    if (generation !== this.generation) return null;
    this.error = error;
    return answer;
  }

  /// Reads the newest page. `manual`: the user asked, so the status says so.
  async refresh(manual: boolean): Promise<void> {
    if (manual) {
      this.manual = true;
      this.note = null;
    }
    if (this.loadingNewest) {
      this.render();
      return;
    }
    this.loadingNewest = true;
    this.render();
    const generation = this.generation;
    const page = await this.read(null);
    if (generation !== this.generation) return;
    this.loadingNewest = false;
    if (page !== null) {
      this.state = withNewest(this.state, page.entries, page.more);
      if (this.manual) this.note = "log.updated";
    }
    this.manual = false;
    this.render();
  }

  private async loadOlder(): Promise<void> {
    const before = olderThan(this.state);
    if (this.loadingOlder || before === null) return;
    this.loadingOlder = true;
    this.render();
    const generation = this.generation;
    const page = await this.read(before);
    if (generation !== this.generation) return;
    this.loadingOlder = false;
    if (page !== null) this.state = withOlder(this.state, page.entries, page.more);
    this.render();
  }

  /// The lines to show: the watchdog's, or under the watchdog-link filter
  /// its start and stop lines merged with this window's records.
  private lines(): Array<readonly [string, LogLineView]> {
    const names = new Map(this.ctx.tunnels());
    const zone = this.ctx.zone();
    const refMs = this.ctx.refMs();
    if (this.filter.kind === "watchdog") {
      return watchdogLines(this.state.entries, this.state.olderExist, this.ctx.linkRecords()).map(
        (line) => [line.key, watchdogLineView(line, names, zone, refMs)] as const,
      );
    }
    return this.state.entries.map((e) => [`w${e.seq}`, entryView(e, names, zone, refMs)] as const);
  }

  /// Redraws from the latest snapshot's names and zone. Cheap enough for
  /// every heartbeat; does nothing while the tab is hidden.
  render(): void {
    this.renderOptions();
    if (!this.active) return;
    const watchdog = this.filter.kind === "watchdog";
    const lines = this.lines();
    this.rows.render(lines);
    const count = lines.length;
    const error = this.error?.() ?? null;
    const state: Key | null = this.state.empty
      ? "log.loading"
      : this.manual
        ? "log.updating"
        : count === 0
          ? "log.empty"
          : this.note;
    showSaid(
      byId("log-state"),
      error !== null
        ? { text: t(watchdog ? "log.error_watchdog" : "log.error", { error: error.text }), detail: error.detail }
        : state === null
          ? null
          : plain(t(state)),
    );
    // Under the watchdog-link filter its own note stands in for the tab's
    // hint.
    setHidden(byId("log-hint"), watchdog);
    const watchNote = byId("log-watch-note");
    setHidden(watchNote, !watchdog);
    setText(
      watchNote,
      watchdog
        ? t("log.watch_note", {
            watchdog: watchdogLabel(),
            gui: guiLabel(),
            kept: this.ctx.linkStored() ? t("log.watch_stored") : t("log.watch_not_stored"),
          })
        : "",
    );
    // Not a live region: it changes with every new line.
    setText(byId("log-count"), count === 0 ? "" : t("log.count", { count }));
    setHidden(this.more, !this.state.olderExist);
    this.more.disabled = this.loadingOlder;
    setText(this.more, this.loadingOlder ? t("log.loading") : t("log.more"));
    setHidden(byId("log-end"), this.state.empty || this.state.olderExist || count === 0);
  }

  /// The filter's choices: all, the lines about no tunnel (when the
  /// watchdog reads them apart), the watchdog's link, and the registered
  /// tunnels. A filter on a tunnel that is gone, or on the system lines
  /// that the watchdog no longer reads apart, goes back to all.
  private renderOptions(): void {
    const tunnels = this.ctx.tunnels();
    const offer = systemOffer(this.ctx.watchdogVersion(), this.systemRefusedBy);
    this.systemRefusedBy = offer.refusedBy;
    const system = offer.offered;
    // Rebuilt for another language too.
    const key = JSON.stringify([language().tag, tunnels, system]);
    if (key === this.optionsKey) return;
    this.optionsKey = key;
    const option = (text: string, value: string) => {
      const o = el("option", { text });
      o.value = value;
      return o;
    };
    const group = el("optgroup");
    group.label = t("log.filter_tunnels");
    group.append(...tunnels.map(([id, name]) => option(name, id)));
    this.select.replaceChildren(
      option(t("log.filter_all"), ""),
      ...(system ? [option(t("log.filter_system"), SYSTEM_OPTION)] : []),
      option(t("log.filter_watchdog"), WATCHDOG_OPTION),
      ...(tunnels.length > 0 ? [group] : []),
    );
    const filter = this.filter;
    if (
      (filter.kind === "tunnel" && !tunnels.some(([id]) => id === filter.tunnel_id)) ||
      (filter.kind === "system" && !system)
    ) {
      this.filter = { kind: "all" };
      this.select.value = "";
      this.restart();
    } else {
      this.select.value = logFilterValue(filter);
    }
  }
}
