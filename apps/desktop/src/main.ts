// The window: five tabs over one snapshot. It renders the view model and
// turns clicks into contract requests; it never decides a tunnel's state.
// Text from the watchdog (tunnel names, error details) is only ever set with
// `textContent`, never parsed as HTML. Cards, discovered tunnels and log
// lines are kept per key and updated in place (`keyed.ts`), and forms are
// only refilled when the user is not editing them, so the heartbeat (every
// 3 to 300 s, 5 by default) never takes focus or typed input away. A
// language change relabels every fixed text in place (`relabel`), says
// every kept status line again (`respeak`) and renders once, so typed input,
// open forms and focus stay as they are.

import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { type CardActions, type TunnelCard, createTunnelCard, updateTunnelCard } from "./card";
import { type WatchdogClient, TauriClient } from "./client";
import { type DiscoveredRow, createDiscoveredRow, updateDiscoveredRow } from "./discovered";
import { type McpServer, aboutView, asMcpServer, mcpConfig, mcpServerText } from "./about";
import {
  CLIENT_IDS,
  CLIENT_NAMES,
  type ClientId,
  type ClientView,
  asClientView,
  asClientViews,
  asNotSetUp,
  checkedText,
  questionText,
  reasonText,
  removeQuestionText,
  stateText,
} from "./mcpclients";
import { MCP_SINCE, RECOVERY_RETRY_SINCE, SETTINGS_FILE_SINCE, compatibilityWarning, offeredSince } from "./compat";
import { type SaveSettings, SettingsFileGroup } from "./settingsfiletab";
import { byId, el, relabel, respeak, setAttr, setControl, setHidden, setText, showSaid, speak, unavailable } from "./dom";
import { inline, noAnswer, refusal as refused, snapshotRefusal, unexpectedAnswer } from "./errors";
import { fitDeviceName, looksLikeBotToken, newRequestId, parseChatId } from "./forms";
import { type HostView, hostView, mcpTelegramView, parseHostSettings } from "./host";
import {
  type Key,
  LANGUAGES,
  type Params,
  SYSTEM_CHOICE,
  asLocales,
  chooseLanguage,
  findLanguage,
  language,
  setLanguage,
  t,
} from "./i18n";
import { LogExport, type SaveExport, type SavedExport } from "./exportlog";
import { KeyedList } from "./keyed";
import { LogTab } from "./logtab";
import {
  REFRESH_SECS,
  loadLanguage,
  loadRefreshSecs,
  lostAfterMs,
  parseRefreshSecs,
  safeStorage,
  saveLanguage,
  saveRefreshSecs,
} from "./prefs";
import type { DiscoveredTunnel, Outcome, Request, TunnelId } from "./protocol";
import { platformOf } from "./report";
import { type FieldValues, confirmFocus, fieldsToClear } from "./sent";
import { type Said, type Words, plain } from "./said";
import { Tabs } from "./tabs";
import { TemplateSync, previewMessages, templateTexts, templateUse, templateUseText, templatesFor } from "./templates";
import { type Zone, formatShort, knownZones, localZoneName, offsetSecsAt } from "./time";
import {
  type DiscoveredView,
  type Link,
  type RegisterAnswer,
  type RowView,
  type SummaryPart,
  ExpiryWatch,
  LatestOnly,
  arrivalAgeMs,
  countdown,
  discoveryView,
  expiredTunnels,
  isLost,
  notificationView,
  messageDeviceName,
  trayCommand,
  viewModel,
} from "./view";
import { LinkJournal, LinkWatch, linkError } from "./watchlog";
import "./styles.css";

/// The watchdog, where log exports are saved, and the window's own version.
/// In a plain browser during development (`pnpm dev`) a made-up one stands
/// in, so the layout can be looked at; the desktop shell always talks to
/// the real one, saves through its own command and reports the desktop
/// binary's version, and the production build leaves the stand-in out
/// (`import.meta.env.DEV` is false there).
async function pickClient(): Promise<{
  client: WatchdogClient;
  save: SaveExport;
  saveSettings: SaveSettings;
  version: () => Promise<string>;
  mcpVersion: () => Promise<unknown>;
  mcpClients: () => Promise<unknown>;
  setUpMcpClient: (client: ClientId) => Promise<unknown>;
  removeMcpClient: (client: ClientId) => Promise<unknown>;
  systemLanguages: () => Promise<unknown>;
}> {
  if (import.meta.env.DEV && !isTauri()) {
    const { FakeClient } = await import("./dev/fake");
    const fake = new FakeClient(window.location.search);
    return {
      client: fake,
      save: (text, offset) => fake.saveExport(text, offset),
      saveSettings: (_document, device, offset) => fake.saveSettings(device, offset),
      version: () => fake.appVersion(),
      mcpVersion: () => fake.mcpVersion(),
      mcpClients: () => fake.mcpClients(),
      setUpMcpClient: (client) => fake.setUpMcpClient(client),
      removeMcpClient: (client) => fake.removeMcpClient(client),
      systemLanguages: () => Promise.resolve(browserLocales()),
    };
  }
  return {
    client: new TauriClient(),
    // The shell picks the folder and the name; only the text and the
    // offset for the name's local time cross over.
    save: (text, utcOffsetMinutes) => invoke<SavedExport>("save_log_export", { text, utcOffsetMinutes }),
    // The watchdog's document, checked again by the shell, and the device
    // name, which only goes into the file's name.
    saveSettings: (document, deviceName, utcOffsetMinutes) =>
      invoke<SavedExport>("save_settings_export", { document, deviceName, utcOffsetMinutes }),
    version: () => invoke<string>("app_version"),
    // The MCP server beside the window, as the shell reads it without
    // running it.
    mcpVersion: () => invoke<unknown>("mcp_version"),
    // The AI tools the shell can add the MCP server to, and adding it to
    // one after the user confirmed: only the tool's name crosses over.
    mcpClients: () => invoke<unknown>("mcp_clients"),
    setUpMcpClient: (client) => invoke<unknown>("set_up_mcp_client", { client }),
    removeMcpClient: (client) => invoke<unknown>("remove_mcp_client", { client }),
    // The system's languages as the shell reads them from the OS: the
    // webview's own list can differ from them (macOS gives an app without
    // localizations English).
    systemLanguages: () => invoke<unknown>("system_languages"),
  };
}

/// The webview's languages, until (or when) the shell has not said the
/// system's.
function browserLocales(): string[] {
  return asLocales(navigator.languages) ?? (navigator.language ? [navigator.language] : []);
}

const {
  client,
  save: saveExport,
  saveSettings,
  version: appVersion,
  mcpVersion,
  mcpClients,
  setUpMcpClient,
  removeMcpClient,
  systemLanguages,
} = await pickClient();
/// The stored language choice: a tag, or following the system (`prefs.ts`).
let languageChoice = loadLanguage();
/// The system's languages, most preferred first: the webview's until the
/// shell has said the OS's.
let systemLocales: readonly string[] = browserLocales();
/// The shell has answered (or failed to): `systemLocales` is final.
let systemKnown = false;
/// How often the window asks for a snapshot, in seconds (`prefs.ts`).
let refreshSecs = loadRefreshSecs();
const link: Link = { snapshot: null, receivedAtMono: null, lastError: null, lostAfterMs: lostAfterMs(refreshSecs) };
/// The desktop binary's version, once the shell has said it; `guiFailed`
/// when it could not.
let guiVersion: string | null = null;
let guiFailed = false;
/// The MCP server beside the window, once the shell has said; `mcpFailed`
/// when it could not, or said something else.
let mcpServer: McpServer | null = null;
let mcpFailed = false;
/// The AI tools' settings as the shell last described them; `null` until
/// it has, `clientsFailed` when it could not.
let clients: ClientView[] | null = null;
let clientsFailed = false;
let pickedClient: ClientId = "claude_code";
/// What the confirmation row asks before: adding the entry or removing it.
let clientAsk: "set_up" | "remove" = "set_up";
const snapshots = new LatestOnly();
/// What this window saw of its link to the watchdog, kept per viewer
/// (`watchlog.ts`): written only when the link changes.
const journal = new LinkJournal(safeStorage(), Date.now());
const watch = new LinkWatch(journal, () => lostAfterMs(refreshSecs));
let discovered: DiscoveredTunnel[] = [];
/// The watchdog's answers to registrations since the last discovery: at most
/// one per discovered tunnel.
const registerAnswers = new Map<TunnelId, RegisterAnswer>();
/// Counts discoveries, so an answer to a registration sent before the latest
/// one is not shown on the new list.
let discoveries = 0;
/// Whether the last render showed the lost-link overlay.
let shownLost = false;
/// The tunnels the last render showed with an expired green, as one key.
let shownExpired = "";
/// When a green light's evidence is about to expire between two snapshots
/// (`ExpiryWatch`): one snapshot is asked for just before.
const expiry = new ExpiryWatch();
/// The zone of the last render, for times written between renders.
let zone: Zone | null = null;
/// Cards in the snapshot's order with their names, for the log's filter.
let tunnelNames: Array<readonly [TunnelId, string]> = [];

/// The window's message line, said again in the new language after a
/// language change.
function say(words: Words): void {
  speak(byId("message"), words);
}

/// A fixed message.
function sayText(key: Key, params?: Params): void {
  say(() => plain(t(key, params)));
}

/// A refusal in words, by its reason or code, the original text in the
/// details; or an answer that is not one.
function refusal(request: Request, outcome: Outcome): Said {
  return "err" in outcome ? refused(request, outcome.err) : unexpectedAnswer();
}

/// `said` with what it is about (a tunnel's name) in front.
function named(label: string, said: Said): Said {
  return { ...said, text: t("common.labelled", { label, text: said.text }) };
}

/// Sends one request. The answer only says it was accepted; the effect shows
/// in the next snapshot, which is fetched right away. `label` names what the
/// request is about (a tunnel, so the message says which of many cards it
/// concerns); `done` words an acceptance, and `quiet` says nothing about
/// one. Returns whether the watchdog accepted it, and the words for the
/// answer.
async function send(
  request: Request,
  { done, label, quiet = false }: { done?: () => string; label?: () => string; quiet?: boolean } = {},
): Promise<{ ok: boolean; words: Words }> {
  let ok = false;
  let words: Words;
  try {
    const outcome = await client.request(newRequestId(), request);
    ok = "ok" in outcome;
    const accepted = done ?? (() => t(request.op === "unregister_tunnel" ? "main.removed" : "common.sent"));
    words = ok ? () => plain(accepted()) : () => refusal(request, outcome);
    void refresh();
  } catch (error) {
    words = () => noAnswer(error);
  }
  if (!(ok && quiet)) say(label === undefined ? words : () => named(label(), words()));
  return { ok, words };
}

const templateSync = new TemplateSync();

/// Whether the window's language is final: chosen, or the system's once
/// the shell has said it. Until then no message templates are sent.
function settleLanguage(): void {
  templateSync.languageSettled = systemKnown || languageChoice !== SYSTEM_CHOICE;
}

/// Hands the watchdog this window's Telegram message templates when the
/// rules of `TemplateSync` say so, judged on a current snapshot only. A
/// refusal (a `storage_unavailable` one, say) is said in the message line;
/// an automatic send that is accepted says nothing.
function syncTemplates(): void {
  const snapshot = link.snapshot;
  if (snapshot === null || isLost(link, performance.now())) return;
  const ours = templatesFor(language());
  if (ours.problems.length > 0) return;
  // A watchdog before 1.2.0 takes no set with the retry and recovery texts;
  // it keeps writing with its own.
  if (!offeredSince(snapshot.host.watchdog_version, RECOVERY_RETRY_SINCE)) return;
  const next = templateSync.next(snapshot.notification.templates_id, ours.set);
  if (next === null) return;
  templateSync.sending = true;
  const name = language().name;
  void send(
    { op: "set_notification_templates", templates: next.set },
    { done: () => t("notify.templates_sent", { name }), label: () => t("notify.templates_label"), quiet: !next.asked },
  ).finally(() => {
    templateSync.sending = false;
  });
}

async function refresh(): Promise<void> {
  const n = snapshots.issue();
  const sentMono = performance.now();
  watch.sent(n, sentMono);
  try {
    const outcome = await client.request(newRequestId(), { op: "get_snapshot" });
    if (!snapshots.accept(n)) return;
    if ("ok" in outcome && outcome.ok.kind === "snapshot") {
      link.snapshot = outcome.ok.snapshot;
      link.receivedAtMono = performance.now();
      link.ageOnArrivalMs = arrivalAgeMs(sentMono, link.receivedAtMono, link.snapshot.generated_at_unix_ms, Date.now());
      link.lastError = null;
      expiry.arrived(link.snapshot, link.receivedAtMono, expiredTunnels(link, link.receivedAtMono));
      watch.answered(n, { ok: true, generatedAtMs: outcome.ok.snapshot.generated_at_unix_ms }, Date.now());
    } else {
      const err = "err" in outcome ? outcome.err : null;
      link.lastError = () => (err === null ? unexpectedAnswer() : snapshotRefusal(err));
      watch.answered(n, { ok: false, error: linkError("err" in outcome ? outcome.err : null) }, Date.now());
    }
  } catch (error) {
    if (!snapshots.accept(n)) return;
    link.lastError = () => noAnswer(error);
    watch.answered(n, { ok: false, error: linkError(null) }, Date.now());
  }
  render();
  // Does nothing unless this snapshot is current.
  syncTemplates();
}

function timingText(kind: string, targetMs: number): string {
  const snapshot = link.snapshot;
  if (!snapshot) return "";
  // A countdown, not a safety signal: the time since the snapshot arrived,
  // without the round trip that `ageOnArrivalMs` counts in to be safe, so a
  // slow answer does not make it run out early.
  const elapsed = link.receivedAtMono === null ? 0 : performance.now() - link.receivedAtMono;
  const left = countdown(targetMs, snapshot.generated_at_unix_ms, elapsed);
  return kind === "deadline" ? t("countdown.deadline", { left }) : t("countdown.next_retry", { left });
}

/// A confirmation dialog for one tunnel. What it will do if the user agrees
/// is set only while it is open, and cleared however it closes (the buttons,
/// Esc or anything else), so a cancelled confirmation can never be carried
/// out later.
function confirmation(name: string): (label: string, onYes: () => void) => void {
  const dialog = byId(`confirm-${name}`) as HTMLDialogElement;
  let pending: (() => void) | null = null;
  byId(`confirm-${name}-yes`).addEventListener("click", () => {
    const action = pending;
    pending = null;
    dialog.close();
    action?.();
  });
  byId(`confirm-${name}-no`).addEventListener("click", () => dialog.close());
  dialog.addEventListener("close", () => {
    pending = null;
  });
  return (label, onYes) => {
    if (dialog.open) return;
    pending = onYes;
    byId(`confirm-${name}-tunnel`).textContent = t("dialog.tunnel", { label });
    dialog.showModal();
  };
}

const cardActions: CardActions = {
  send: (request, label) => send(request, { label: () => label }),
  // The advanced option of plan §6.4 needs an explicit confirmation first.
  confirmExternal: confirmation("external"),
  // Removal says first that the WireGuard tunnel itself is not touched.
  confirmRemove: confirmation("remove"),
  removed: () => byId("tunnels-title").focus(),
  timingText,
  time: (unixMs, refMs) => formatShort(unixMs, refMs, zone),
};

const cards = new KeyedList<TunnelId, RowView, Node, TunnelCard>(
  byId("tunnels"),
  (id) => createTunnelCard(id, cardActions),
  (card, view) => updateTunnelCard(card, view, cardActions),
);

/// Registers a discovered tunnel. Whether it is on the installation's access
/// list is shown only as the watchdog answers it (plan §9.2).
async function register(id: TunnelId, label: string): Promise<void> {
  const request: Request = { op: "register_tunnel", tunnel_id: id };
  const discovery = discoveries;
  registerAnswers.set(id, { kind: "pending" });
  render();
  let answer: RegisterAnswer;
  try {
    const outcome = await client.request(newRequestId(), request);
    answer = "ok" in outcome ? { kind: "accepted" } : { kind: "refused", words: () => refusal(request, outcome) };
  } catch (error) {
    answer = {
      kind: "no_answer",
      words: () => {
        const said = noAnswer(error);
        return { ...said, text: t("tunnels.register_no_answer", { text: said.text }) };
      },
    };
  }
  // A discovery since then started over; its list does not get this answer.
  if (discovery === discoveries) registerAnswers.set(id, answer);
  const result = answer;
  say(() => named(label, "words" in result ? result.words() : plain(t("tunnels.register_accepted"))));
  void refresh();
}

const discoveredRows = new KeyedList<TunnelId, DiscoveredView, Node, DiscoveredRow>(
  byId("discovered"),
  (id) => createDiscoveredRow(id, (tunnel, label) => void register(tunnel, label)),
  updateDiscoveredRow,
);

const logTab = new LogTab({
  ask: (request) => client.request(newRequestId(), request),
  refusal,
  noAnswer,
  tunnels: () => tunnelNames,
  zone: () => zone,
  refMs: () => link.snapshot?.generated_at_unix_ms ?? Date.now(),
  linkRecords: () => journal.list(),
  linkStored: () => journal.persistent,
  watchdogVersion: () => (isLost(link, performance.now()) ? null : (link.snapshot?.host.watchdog_version ?? null)),
});

new LogExport({
  ask: (request) => client.request(newRequestId(), request),
  refusal,
  snapshot: () => link.snapshot,
  lost: () => isLost(link, performance.now()),
  // The export's text has no collapsed part: the original goes in
  // parentheses.
  lastError: () => (link.lastError === null ? null : inline(link.lastError())),
  guiVersion: () => guiVersion,
  linkRecords: () => journal.list(),
  linkStored: () => journal.persistent,
  save: saveExport,
  say,
});

const settingsFile = new SettingsFileGroup({
  ask: (request) => client.request(newRequestId(), request),
  refusal,
  noAnswer,
  snapshot: () => link.snapshot,
  offered: () =>
    !isLost(link, performance.now()) &&
    offeredSince(link.snapshot?.host.watchdog_version ?? null, SETTINGS_FILE_SINCE),
  offsetMinutes: () => Math.round(offsetSecsAt(Date.now(), zone) / 60),
  save: saveSettings,
});

/// The about tab: both versions, both minimums and whether they work
/// together; and the header's warning, on every tab, while they do not.
function renderAbout(lost: boolean): void {
  const view = aboutView(link.snapshot, lost, guiVersion, navigator.userAgent, { guiFailed, mcp: mcpServer, mcpFailed });
  const facts = byId("about-facts");
  const key = JSON.stringify(view.facts);
  if (facts.dataset.key !== key) {
    facts.dataset.key = key;
    facts.replaceChildren(...view.facts.map(([k, v]) => el("div", {}, el("dt", { text: k }), el("dd", { text: v }))));
  }
  // Replaced only when it says something new: it is a status region.
  const verdict = byId("about-compat");
  const lines = JSON.stringify(view.lines);
  if (verdict.dataset.key !== lines) {
    verdict.dataset.key = lines;
    verdict.replaceChildren(...view.lines.map((line) => el("p", { text: line })));
  }
  setAttr(verdict, "class", `compat-verdict ${view.compatibility.kind}`);
  const warning = compatibilityWarning(view.compatibility);
  setHidden(byId("compat-warning"), warning === null);
  setText(byId("compat-warning-text"), warning ?? "");
}

/// The window group of the settings tab, and the log tab's word for it.
function renderRefresh(): void {
  setText(
    byId("refresh-current"),
    t("settings.refresh_current", { secs: refreshSecs, lost: lostAfterMs(refreshSecs) / 1000 }),
  );
  setText(byId("log-hint"), t("log.hint", { secs: refreshSecs }));
}

let heartbeat: ReturnType<typeof setInterval> | null = null;

/// Asks for a snapshot, and refreshes an open log tab, every interval.
function startHeartbeat(): void {
  if (heartbeat !== null) clearInterval(heartbeat);
  heartbeat = setInterval(() => {
    void refresh();
    logTab.tick();
  }, refreshSecs * 1000);
}

/// Applies a new interval at once: the heartbeat, the lost-link threshold
/// and the tray's staleness follow it. Returns whether it was remembered.
function applyRefreshSecs(secs: number): boolean {
  refreshSecs = secs;
  link.lostAfterMs = lostAfterMs(secs);
  const stored = saveRefreshSecs(secs);
  (byId("refresh-secs") as HTMLInputElement).value = String(secs);
  startHeartbeat();
  renderRefresh();
  void refresh();
  return stored;
}

const tabs = new Tabs((tab) => logTab.show(tab === "log"));

/// Inputs the user has changed since they were last filled: never
/// overwritten from a snapshot until saved or reset.
function editing(input: HTMLElement): boolean {
  return document.activeElement === input || input.dataset.dirty === "1";
}

function markDirty(...ids: string[]): void {
  for (const id of ids) {
    byId(id).addEventListener("input", (e) => {
      (e.currentTarget as HTMLElement).dataset.dirty = "1";
    });
  }
}

function clearDirty(...ids: string[]): void {
  for (const id of ids) delete byId(id).dataset.dirty;
}

/// What the fields hold now: an input's text, a check box's or radio
/// button's state.
function fieldValues(...ids: string[]): FieldValues {
  const values: Record<string, string> = {};
  for (const id of ids) {
    const input = byId(id) as HTMLInputElement;
    values[id] = input.type === "checkbox" || input.type === "radio" ? String(input.checked) : input.value;
  }
  return values;
}

/// Once the answer to a form is in: clears the "changed" marks of the
/// fields that still hold what was sent, and only if it was accepted
/// (FIX-07, AR-15). A refusal keeps what was typed; anything typed while
/// the request was out is kept too.
function settle(sent: FieldValues, ok: boolean): void {
  clearDirty(...fieldsToClear(sent, fieldValues(...Object.keys(sent)), ok));
}

function renderNotifications(): void {
  const n = notificationView(link.snapshot);
  const enabled = byId("notify-enabled") as HTMLInputElement;
  const device = byId("notify-device") as HTMLInputElement;
  const chat = byId("notify-chat") as HTMLInputElement;
  if (!editing(enabled)) enabled.checked = n.enabled;
  if (!editing(device)) device.value = n.deviceName;
  device.placeholder = n.deviceHost;
  setText(byId("notify-device-note"), n.deviceHost ? `${t("notify.device_note")} ${n.deviceHost}` : t("notify.device_note"));
  if (!editing(chat)) chat.value = n.chatId;
  setText(byId("notify-token-state"), t("common.current", { value: n.token }));
  setText(byId("notify-test-state"), n.test ? t("notify.last_test", { result: n.test }) : "");
  setText(byId("notify-failure"), n.lastFailure ? t("notify.last_failure", { failure: n.lastFailure }) : "");
  (byId("notify-test") as HTMLButtonElement).disabled = n.testBusy || link.snapshot === null;
  renderTemplates(isLost(link, performance.now()));
}

/// The notify tab's templates group: which set the watchdog uses, the button
/// that hands it this window's over another GUI's choice, and a preview of
/// both messages from this window's set with sample values.
function renderTemplates(lost: boolean): void {
  const lang = language();
  const ours = templatesFor(lang);
  const snapshot = link.snapshot;
  const use = templateUse(lost || snapshot === null ? undefined : snapshot.notification.templates_id, ours.set.id);
  showSaid(
    byId("templates-state"),
    ours.problems.length > 0
      ? { text: t("notify.templates_broken"), detail: ours.problems.join("\n") }
      : plain(templateUseText(use, lang)),
  );
  setHidden(byId("templates-foreign"), use.kind !== "foreign");
  const nowMs = snapshot?.generated_at_unix_ms ?? Date.now();
  const preview = previewMessages(templateTexts(lang), {
    deviceName: messageDeviceName(snapshot?.notification),
    tunnel: snapshot?.tunnels[0]?.display_name ?? t("notify.preview_tunnel"),
    incident: PREVIEW_INCIDENT,
    startedAtMs: nowMs - 5 * 60_000,
    nowMs,
    zone: snapshot?.host.time_zone ?? null,
    retry: snapshot?.tunnels[0]?.retry,
  });
  setText(byId("preview-formal"), preview.formal);
  setText(byId("preview-exhausted"), preview.exhausted);
  setText(byId("preview-recovered"), preview.recovered);
  setText(byId("preview-test"), preview.test);
}

/// The incident ID the preview shows, in the watchdog's form.
const PREVIEW_INCIDENT = "inc-000042";

const HOST_FIELDS = ["workers-auto", "workers-manual", "workers-count", "zone-follow", "zone-pick", "zone-name"];
let hostFactsKey = "";

/// Fills one choice (two radios and the value beside them) unless the user
/// is changing any part of it: typing a value also picks its radio.
function fillChoice(ids: [string, string, string], first: boolean, text: string, force: boolean): void {
  const [a, b, value] = ids.map((id) => byId(id) as HTMLInputElement) as [
    HTMLInputElement,
    HTMLInputElement,
    HTMLInputElement,
  ];
  if (!force && (editing(a) || editing(b) || editing(value))) return;
  a.checked = first;
  b.checked = !first;
  value.value = text;
}

function fillHost(view: HostView, force: boolean): void {
  fillChoice(["workers-auto", "workers-manual", "workers-count"], view.workers.auto, view.workers.count, force);
  fillChoice(["zone-follow", "zone-pick", "zone-name"], view.zone.follow, view.zone.name, force);
  if (force) clearDirty(...HOST_FIELDS);
  showChoices();
}

/// The count and the zone name look inactive while the automatic count or
/// the system's zone is chosen. They stay typable: typing in one chooses
/// it.
function showChoices(): void {
  const dim = (radio: string, field: string) =>
    byId(field).classList.toggle("inactive", (byId(radio) as HTMLInputElement).checked);
  dim("workers-auto", "workers-count-field");
  dim("zone-follow", "zone-name-field");
}

function renderHost(lost: boolean): void {
  const view = hostView(link.snapshot, lost, localZoneName());
  const paused = link.snapshot?.monitoring_paused ?? false;
  const pause = byId("pause");
  setText(pause, paused ? t("settings.resume_all") : t("settings.pause_all"));
  setControl(pause, lost || link.snapshot === null ? { kind: "disabled", why: t("common.lost") } : { kind: "enabled" });
  setText(
    byId("pause-state"),
    link.snapshot === null
      ? t("common.unknown_state")
      : paused
        ? t("settings.pause_state_paused")
        : t("settings.pause_state_running"),
  );
  renderMcp(lost);
  const save = byId("host-form").querySelector<HTMLButtonElement>("button[type=submit]");
  if (view === null) {
    if (save) setControl(save, { kind: "disabled", why: t("common.lost") });
    return;
  }
  if (save) setControl(save, view.save);
  setText(byId("workers-auto-label"), view.workers.autoLabel);
  setText(byId("zone-follow-label"), view.zone.followLabel);
  setText(byId("zone-current"), view.zone.current);
  const count = byId("workers-count") as HTMLInputElement;
  setAttr(count, "min", String(view.workers.min));
  setAttr(count, "max", String(view.workers.max));
  const factsKey = JSON.stringify(view.workers.facts);
  if (factsKey !== hostFactsKey) {
    hostFactsKey = factsKey;
    byId("workers-facts").replaceChildren(
      ...view.workers.facts.map(([k, v]) => el("div", {}, el("dt", { text: k }), el("dd", { text: v }))),
    );
  }
  fillHost(view, false);
}

/// The header's summary line, each count in its light's colour; rebuilt
/// only when a part or its colour changed.
let shownSummary = "";
function renderSummary(parts: readonly SummaryPart[]): void {
  const key = JSON.stringify(parts);
  if (key === shownSummary) return;
  shownSummary = key;
  byId("summary").replaceChildren(
    ...parts.map((part) => (part.tone === null ? part.text : el("span", { className: `tone-${part.tone}`, text: part.text }))),
  );
}

function render(): void {
  const now = performance.now();
  const vm = viewModel(link, now);
  const lost = vm.overlay !== null;
  zone = vm.zone;
  shownLost = lost;
  // Tunnel IDs have no spaces (`[A-Za-z0-9_-]`).
  shownExpired = vm.rows
    .filter((r) => r.expired !== null)
    .map((r) => r.id)
    .join(" ");
  const overlay = byId("overlay");
  setHidden(overlay, !lost);
  // Replaced only when it says something new: it is an alert region.
  const overlayText = vm.overlay ? JSON.stringify(vm.overlay) : "";
  if (overlay.dataset.text !== overlayText) {
    overlay.dataset.text = overlayText;
    const body = el("div");
    if (vm.overlay) showSaid(body, { text: vm.overlay.text, detail: vm.overlay.detail });
    overlay.replaceChildren(...(vm.overlay ? [el("strong", { text: vm.overlay.title }), body] : []));
  }
  setHidden(byId("banner"), vm.banner === null);
  setText(byId("banner-text"), vm.banner ?? "");
  setHidden(byId("banner-resume"), !(vm.paused && !lost));
  const pill = byId("link");
  setText(pill, vm.link.text);
  setAttr(pill, "class", `link link-${vm.link.monitor}`);
  renderSummary(vm.summaryParts);

  tunnelNames = vm.rows.map((r) => [r.id, r.label] as const);
  cards.render(link.snapshot ? vm.rows.map((row) => [row.id, row] as const) : []);
  setHidden(byId("tunnels-empty"), !(link.snapshot && vm.rows.length === 0));
  setControl(byId("discover"), lost ? { kind: "disabled", why: t("common.lost") } : { kind: "enabled" });
  const discovery = discoveryView(discovered, link.snapshot, registerAnswers, lost, discoveries > 0);
  discoveredRows.render(discovery.entries.map((entry) => [entry.id, entry] as const));
  const note = byId("discovered-note");
  setHidden(note, discovery.note === null);
  setText(note, discovery.note ?? "");
  setHidden(byId("add"), discovered.length === 0 && discoveries === 0);

  renderNotifications();
  renderHost(lost);
  renderAbout(lost);
  logTab.render();
  settingsFile.render();

  if (isTauri()) {
    void invoke("update_tray", {
      summary: trayCommand(vm.tray, refreshSecs),
      // The shell words the tray from its own copy of the language files;
      // only the tag crosses, never a text.
      language: language().tag,
    }).catch(() => undefined);
  }
}

/// The agents' switch (owner decision of 2026-11-04): what it is, and the
/// button that turns it the other way, offered only by a watchdog that has it.
function renderMcp(lost: boolean): void {
  const snapshot = link.snapshot;
  const offered = !lost && snapshot !== null && offeredSince(snapshot.host.watchdog_version, MCP_SINCE);
  const on = offered && snapshot.host.mcp_enabled;
  const toggle = byId("mcp-toggle");
  setText(toggle, on ? t("mcp.turn_off") : t("mcp.turn_on"));
  setControl(toggle, offered ? { kind: "enabled" } : { kind: "disabled", why: t("mcp.not_offered") });
  setText(byId("mcp-state"), !offered ? t("mcp.not_offered") : on ? t("mcp.on") : t("mcp.off"));
  const watchdogVersion = !lost && snapshot !== null ? snapshot.host.watchdog_version : null;
  setText(byId("mcp-version"), t("mcp.version", { version: mcpServerText(mcpServer, mcpFailed, watchdogVersion) }));
  setText(byId("mcp-config"), mcpConfig(platformOf(navigator.userAgent)));
  const telegram = mcpTelegramView(snapshot, lost);
  const telegramToggle = byId("mcp-telegram-toggle");
  setText(telegramToggle, telegram.toggle);
  setControl(telegramToggle, telegram.control);
  setText(byId("mcp-telegram-state"), telegram.state);
  renderMcpClients();
}

/// The one-click client setup: the picked tool, what its settings say, and
/// the entry that is written. Local to this computer, so it does not wait
/// for the watchdog.
function renderMcpClients(): void {
  const select = byId("mcp-client") as HTMLSelectElement;
  if (select.options.length !== CLIENT_IDS.length) {
    select.replaceChildren(
      ...CLIENT_IDS.map((id) => {
        const option = document.createElement("option");
        option.value = id;
        option.textContent = CLIENT_NAMES[id];
        return option;
      }),
    );
  }
  select.value = pickedClient;
  const view = clients?.find((c) => c.client === pickedClient) ?? null;
  const installed = mcpServer?.state === "installed";
  setText(
    byId("mcp-client-state"),
    view !== null
      ? stateText(view)
      : clientsFailed
        ? t("mcp.clients_unavailable")
        : t("mcp.clients_reading"),
  );
  setText(byId("mcp-client-preview"), view?.preview ?? "");
  const reading = clientsFailed ? t("mcp.clients_unavailable") : t("mcp.clients_reading");
  setControl(
    byId("mcp-client-set-up"),
    view === null
      ? { kind: "disabled", why: reading }
      : !installed
        ? { kind: "disabled", why: t("mcp.clients_no_server") }
        : { kind: "enabled" },
  );
  // Removing needs no installed server: it also cleans up after WGAL.
  setControl(
    byId("mcp-client-remove"),
    view === null
      ? { kind: "disabled", why: reading }
      : view.state === "configured" || view.state === "other"
        ? { kind: "enabled" }
        : { kind: "disabled", why: stateText(view) },
  );
}

function askToSetUpClient(asking: boolean, mode: "set_up" | "remove" = clientAsk): void {
  const view = clients?.find((c) => c.client === pickedClient) ?? null;
  const ask = asking && view !== null;
  clientAsk = mode;
  if (view !== null) {
    setText(byId("mcp-client-question"), mode === "remove" ? removeQuestionText(view) : questionText(view));
  }
  setText(byId("mcp-client-yes"), mode === "remove" ? t("mcp.clients_remove_confirm") : t("mcp.clients_confirm"));
  const wasOpen = !byId("mcp-client-confirm").hidden;
  setHidden(byId("mcp-client-buttons"), ask);
  setHidden(byId("mcp-client-confirm"), !ask);
  // Only when the row opens or closes: picking another tool or pressing
  // "check" leaves the focus where it is (FIX-07, AR-15).
  const focus = confirmFocus(wasOpen, ask, mode);
  if (focus !== null) byId(focus).focus();
}

/// Reads every tool's settings again and says what the picked one has.
async function checkClient(): Promise<void> {
  askToSetUpClient(false);
  await loadMcpClients();
  const view = clients?.find((c) => c.client === pickedClient) ?? null;
  if (view === null) sayText("mcp.clients_unavailable");
  else say(() => plain(checkedText(view)));
}

async function loadMcpClients(): Promise<void> {
  try {
    const answer = asClientViews(await mcpClients());
    clients = answer;
    clientsFailed = answer === null;
  } catch {
    clientsFailed = true;
  }
  render();
}

async function setUpClient(): Promise<void> {
  const client = pickedClient;
  const removing = clientAsk === "remove";
  const path = clients?.find((c) => c.client === client)?.path ?? "";
  askToSetUpClient(false);
  try {
    const view = asClientView(await (removing ? removeMcpClient(client) : setUpMcpClient(client)));
    if (view === null || view.client !== client) {
      say(() => plain(reasonText("io", client, path)));
    } else {
      clients = (clients ?? []).filter((c) => c.client !== client).concat(view);
      sayText(removing ? "mcp.clients_removed" : "mcp.clients_done", { client: CLIENT_NAMES[client] });
    }
  } catch (error) {
    say(() => plain(reasonText(asNotSetUp(error), client, path)));
  }
  await loadMcpClients();
}

function toggleMcp(): void {
  const snapshot = link.snapshot;
  if (isLost(link, performance.now()) || snapshot === null) {
    sayText("settings.pause_lost");
    return;
  }
  void send({ op: "set_mcp_enabled", enabled: !snapshot.host.mcp_enabled });
}

/// Lets agents set Telegram, or stops letting them (owner decision of
/// 2026-11-06); offered only while MCP is on.
function toggleMcpTelegram(): void {
  const snapshot = link.snapshot;
  if (isLost(link, performance.now()) || snapshot === null) {
    sayText("settings.pause_lost");
    return;
  }
  void send({ op: "set_mcp_telegram_enabled", enabled: !snapshot.host.mcp_telegram_enabled });
}

/// The token's clear button asks first (owner request of 2026-11-06): it
/// gives way to a red confirm button and a cancel button, and only the
/// confirm button clears.
function askToClearToken(asking: boolean): void {
  setHidden(byId("token-buttons"), asking);
  setHidden(byId("token-clear-confirm"), !asking);
  byId(asking ? "token-clear-no" : "token-clear").focus();
}

function togglePause(): void {
  // Nothing is toggled from a state that is not known to be current.
  if (isLost(link, performance.now()) || link.snapshot === null) {
    sayText("settings.pause_lost");
    return;
  }
  void send({ op: "set_monitoring_paused", paused: !link.snapshot.monitoring_paused });
}

async function discover(): Promise<void> {
  const request: Request = { op: "discover_tunnels" };
  sayText("tunnels.discovering");
  try {
    const outcome = await client.request(newRequestId(), request);
    if ("ok" in outcome && outcome.ok.kind === "discovered") {
      discovered = outcome.ok.tunnels;
      discoveries += 1;
      registerAnswers.clear();
      const count = discovered.length;
      sayText(count === 0 ? "tunnels.discovered_none" : "tunnels.discovered_some", { count });
    } else {
      say(() => refusal(request, outcome));
    }
  } catch (error) {
    say(() => noAnswer(error));
  }
  render();
  if (discoveries > 0) {
    // The list is below every registered tunnel: bring it into view and
    // continue keyboard navigation from there.
    const heading = byId("discovered-title");
    heading.scrollIntoView({ block: "start" });
    heading.focus({ preventScroll: true });
  }
}

function wire(): void {
  for (const id of ["pause", "banner-resume"]) {
    byId(id).addEventListener("click", (e) => {
      if (!unavailable(e.currentTarget as Element)) togglePause();
    });
  }
  byId("mcp-toggle").addEventListener("click", (e) => {
    if (!unavailable(e.currentTarget as Element)) toggleMcp();
  });
  byId("mcp-telegram-toggle").addEventListener("click", (e) => {
    if (!unavailable(e.currentTarget as Element)) toggleMcpTelegram();
  });
  byId("mcp-client").addEventListener("change", (e) => {
    const value = (e.currentTarget as HTMLSelectElement).value;
    const picked = CLIENT_IDS.find((id) => id === value);
    if (picked !== undefined) pickedClient = picked;
    askToSetUpClient(false);
    renderMcpClients();
  });
  byId("mcp-client-set-up").addEventListener("click", (e) => {
    if (!unavailable(e.currentTarget as Element)) askToSetUpClient(true, "set_up");
  });
  byId("mcp-client-remove").addEventListener("click", (e) => {
    if (!unavailable(e.currentTarget as Element)) askToSetUpClient(true, "remove");
  });
  byId("mcp-client-check").addEventListener("click", () => void checkClient());
  byId("mcp-client-no").addEventListener("click", () => askToSetUpClient(false));
  byId("mcp-client-yes").addEventListener("click", () => void setUpClient());
  byId("compat-warning-about").addEventListener("click", () => tabs.select("about", true));
  byId("discover").addEventListener("click", (e) => {
    if (!unavailable(e.currentTarget as Element)) void discover();
  });

  markDirty("notify-enabled", "notify-device", "notify-chat");
  byId("notify-form").addEventListener("submit", (event) => {
    event.preventDefault();
    const device = byId("notify-device") as HTMLInputElement;
    const chat = byId("notify-chat") as HTMLInputElement;
    const chatText = chat.value.trim();
    let chatId: string | null = null;
    if (chatText !== "") {
      const parsed = parseChatId(chatText);
      if (!parsed.ok) return say(() => plain(parsed.error));
      chatId = parsed.value;
    }
    // Any name the user likes, as typed (not trimmed), cut to 64
    // characters with "..." (owner decision of 2026-11-06); the field shows
    // what is sent.
    const deviceName = fitDeviceName(device.value);
    device.value = deviceName;
    // The marks are cleared once the watchdog accepted, never before: a
    // refusal would otherwise fill the old values over what was typed
    // (FIX-07, AR-15).
    const sent = fieldValues("notify-enabled", "notify-device", "notify-chat");
    void send({
      op: "update_notification_settings",
      enabled: (byId("notify-enabled") as HTMLInputElement).checked,
      device_name: deviceName,
      chat_id: chatId,
    }).then(({ ok }) => settle(sent, ok));
  });
  byId("token-form").addEventListener("submit", (event) => {
    event.preventDefault();
    const input = byId("notify-token") as HTMLInputElement;
    // The token goes to the watchdog once and is cleared here at once; it is
    // never kept, logged or stored by the GUI (plan §7.1).
    const token = input.value.trim();
    input.value = "";
    if (!looksLikeBotToken(token)) return sayText("notify.token_invalid");
    void send({ op: "set_bot_token", token }, { done: () => t("notify.token_sent") });
  });
  byId("token-clear").addEventListener("click", () => askToClearToken(true));
  byId("token-clear-no").addEventListener("click", () => askToClearToken(false));
  byId("token-clear-yes").addEventListener("click", () => {
    askToClearToken(false);
    void send({ op: "clear_bot_token" });
  });
  byId("notify-test").addEventListener("click", () => {
    (byId("notify-test") as HTMLButtonElement).disabled = true;
    void send({ op: "send_test_notification" }, { done: () => t("notify.test_queued") });
  });

  markDirty(...HOST_FIELDS);
  // Typing a count or a zone chooses it.
  byId("workers-count").addEventListener("input", () => {
    (byId("workers-manual") as HTMLInputElement).checked = true;
  });
  byId("zone-name").addEventListener("input", () => {
    (byId("zone-pick") as HTMLInputElement).checked = true;
  });
  byId("host-form").addEventListener("input", showChoices);
  byId("host-reset").addEventListener("click", () => {
    const view = hostView(link.snapshot, isLost(link, performance.now()), localZoneName());
    if (view) fillHost(view, true);
    speak(byId("host-status"), null);
  });
  byId("host-form").addEventListener("submit", (event) => {
    event.preventDefault();
    const status = byId("host-status");
    const save = byId("host-form").querySelector<HTMLButtonElement>("button[type=submit]");
    if (link.snapshot === null || (save && unavailable(save))) {
      speak(status, () => plain(t("settings.save_lost")));
      return;
    }
    const parsed = parseHostSettings(
      {
        workersAuto: (byId("workers-auto") as HTMLInputElement).checked,
        workers: (byId("workers-count") as HTMLInputElement).value,
        zoneFollow: (byId("zone-follow") as HTMLInputElement).checked,
        zone: (byId("zone-name") as HTMLInputElement).value,
      },
      link.snapshot,
    );
    if (!parsed.ok) {
      speak(status, () => plain(parsed.error));
      return;
    }
    speak(status, () => plain(t("common.sending")));
    const sent = fieldValues(...HOST_FIELDS);
    void send({ op: "update_host_settings", settings: parsed.value }).then(({ ok, words }) => {
      settle(sent, ok);
      speak(status, ok ? () => plain(t("common.accepted")) : words);
    });
  });
  byId("ui-form").addEventListener("submit", (event) => {
    event.preventDefault();
    const secs = parseRefreshSecs((byId("refresh-secs") as HTMLInputElement).value);
    const status = byId("refresh-status");
    if (secs === null) {
      speak(status, () => plain(t("settings.refresh_invalid", { min: REFRESH_SECS.min, max: REFRESH_SECS.max })));
      return;
    }
    const stored = applyRefreshSecs(secs);
    speak(status, () => plain(t(stored ? "settings.refresh_applied" : "settings.refresh_applied_unsaved", { secs })));
  });
  byId("refresh-default").addEventListener("click", () => {
    const stored = applyRefreshSecs(REFRESH_SECS.default);
    const secs = REFRESH_SECS.default;
    speak(
      byId("refresh-status"),
      () => plain(t(stored ? "settings.refresh_defaulted" : "settings.refresh_defaulted_unsaved", { secs })),
    );
  });
  const picker = byId("language") as HTMLSelectElement;
  const system = el("option");
  system.value = SYSTEM_CHOICE;
  picker.replaceChildren(
    system,
    ...LANGUAGES.map((l) => {
      // Each language by its own name, whatever the window speaks.
      const option = el("option", { text: l.name });
      option.value = l.tag;
      return option;
    }),
  );
  labelSystemChoice();
  picker.value = languageChoice;
  picker.addEventListener("change", () => {
    languageChoice = picker.value === SYSTEM_CHOICE ? SYSTEM_CHOICE : findLanguage(picker.value).tag;
    settleLanguage();
    const stored = saveLanguage(languageChoice);
    applyLanguage(chooseLanguage(languageChoice, systemLocales).tag);
    speak(byId("language-status"), () =>
      plain(t(stored ? "settings.language_applied" : "settings.language_applied_unsaved", { name: language().name })),
    );
    // The user chose a language: its message templates go to the watchdog,
    // over another GUI's choice too.
    templateSync.request();
    syncTemplates();
  });
  byId("templates-use-ours").addEventListener("click", () => {
    templateSync.request();
    syncTemplates();
  });
  (byId("refresh-secs") as HTMLInputElement).value = String(refreshSecs);
  renderRefresh();
  byId("zone-list").replaceChildren(
    ...knownZones().map((name) => {
      const option = el("option");
      option.value = name;
      return option;
    }),
  );

  if (isTauri()) {
    void listen("wgal://toggle-pause", togglePause);
    void listen("wgal://explain-stop", () => {
      tabs.select("settings");
      sayText("main.explain_stop");
    });
  }
}

/// Every second: countdowns move, and the lost-link overlay appears even when
/// no snapshot arrives. A newly lost link redraws the cards without their
/// lights (plan §6.1). Just before a green light's evidence expires one
/// snapshot is asked for (`ExpiryWatch`), and once it has expired the green
/// is redrawn as expired, which also drops the tray's green dot. One timer
/// for the whole window, whatever the number of tunnels.
function tick(): void {
  for (const span of document.querySelectorAll<HTMLElement>(".timing[data-at]")) {
    setText(span, timingText(span.dataset.kind ?? "", Number(span.dataset.at)));
  }
  // A snapshot request out for too long is a lost link in the journal.
  const recorded = watch.check(performance.now(), Date.now());
  if (expiry.shouldRefresh(link, performance.now())) void refresh();
  const expired = expiredTunnels(link, performance.now());
  if (isLost(link, performance.now()) !== shownLost || expired.join(" ") !== shownExpired) render();
  else if (recorded) logTab.render();
}

/// Speaks `tag`'s language from now on: every fixed text is relabelled in
/// place, every kept status line said again and everything rendered, in
/// one pass, so typed input, open forms and focus stay as they are. The
/// tray follows with the render (`update_tray` carries the tag).
function applyLanguage(tag: string): void {
  const now = setLanguage(tag);
  document.documentElement.lang = now.tag;
  relabel();
  labelSystemChoice();
  renderRefresh();
  render();
  // After the render, so words that read a rendered value (a control's
  // reason for being unavailable) read the new one.
  respeak();
}

// The settings' fixed texts that name the refresh limits.
byId("refresh-label").dataset.i18nParams = JSON.stringify({ min: REFRESH_SECS.min, max: REFRESH_SECS.max });
byId("refresh-default").dataset.i18nParams = JSON.stringify({ secs: REFRESH_SECS.default });
/// The picker's first entry, which follows the system: it names the
/// language that gives, in the language the window speaks.
function labelSystemChoice(): void {
  const option = (byId("language") as HTMLSelectElement).options.item(0);
  if (option?.value === SYSTEM_CHOICE) {
    setText(option, t("settings.language_system", { name: chooseLanguage(SYSTEM_CHOICE, systemLocales).name }));
  }
}

setLanguage(chooseLanguage(languageChoice, systemLocales).tag);
settleLanguage();
relabel();
document.documentElement.lang = language().tag;
wire();
render();
void refresh();
startHeartbeat();
setInterval(tick, 1_000);
appVersion()
  .then((version) => {
    guiVersion = version;
  })
  .catch(() => {
    guiFailed = true;
  })
  .finally(render);
// The OS's languages from the shell: when the window follows the system and
// they give another language than the webview's did, it switches now. A
// valid empty list means the system names no language, so English; a
// failed or malformed answer, or none within 3 s, leaves the webview's.
// Only then may the window's message templates go to the watchdog.
Promise.race([systemLanguages(), new Promise((resolve) => setTimeout(() => resolve(null), 3_000))])
  .then((answer) => {
    const locales = asLocales(answer);
    if (locales !== null) systemLocales = locales;
  })
  .catch(() => undefined)
  .finally(() => {
    systemKnown = true;
    settleLanguage();
    const tag = chooseLanguage(languageChoice, systemLocales).tag;
    if (tag !== language().tag) applyLanguage(tag);
    else labelSystemChoice();
    syncTemplates();
  });
void loadMcpClients();
mcpVersion()
  .then((answer) => {
    mcpServer = asMcpServer(answer);
    mcpFailed = mcpServer === null;
  })
  .catch(() => {
    mcpFailed = true;
  })
  .finally(render);
