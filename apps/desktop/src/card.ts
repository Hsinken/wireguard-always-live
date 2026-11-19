// One registered tunnel as a card: name, light and reason first, then its
// numbers, actions and switches, and two collapsible parts, its settings and
// its details. Each card is built once per tunnel, with its listeners, and
// then updated in place from every snapshot (see `keyed.ts`), so a heartbeat
// never takes keyboard focus, the scroll position, an open part or typed
// input away. The settings form is filled from the watchdog's values only
// when it is opened or reset, never while the user types in it.

import { type Parsed, parseObserveTargets, parseRetry, parseTarget, parseTiming } from "./forms";
import { type Key, t } from "./i18n";
import {
  MAX_OBSERVE_TARGETS,
  PROBE_INTERVAL_SECS,
  PROBE_MIN_FAULT_SECS,
  PROBE_RETRIES,
  PROBE_TIMEOUT_SECS,
  RECOVERY_RETRY_INTERVAL_SECS,
  RECOVERY_RETRY_LIMIT,
  type ProbeTarget,
  type ProbeTiming,
  type RecoveryRetry,
  type Request,
  type TunnelId,
} from "./protocol";
import {
  actionButton,
  el,
  field,
  labelAttr,
  setAttr,
  setControl,
  setHidden,
  setText,
  showSaid,
  speak,
  switchControl,
  uid,
  unavailable,
} from "./dom";
import { type Words, plain } from "./said";
import { judgmentText } from "./timing";
import type { RowView, SettingsView, SwitchView } from "./view";

/// What a card asks of the window. `label` is the card's name, for messages
/// that say which tunnel they are about.
export interface CardActions {
  send(request: Request, label: string): Promise<{ ok: boolean; words: Words }>;
  confirmExternal(label: string, onYes: () => void): void;
  confirmRemove(label: string, onYes: () => void): void;
  /// The watchdog accepted a removal: the card goes with the next snapshot,
  /// so keyboard focus moves somewhere that stays.
  removed(): void;
  timingText(kind: string, targetMs: number): string;
  /// A time in the configured zone, short when it is on the day of `refMs`.
  time(unixMs: number, refMs: number): string;
}

interface CardSwitch {
  label: HTMLLabelElement;
  box: HTMLInputElement;
  note: HTMLElement;
}

interface TimingField {
  input: HTMLInputElement;
  current: HTMLElement;
}

export interface TunnelCard {
  readonly node: HTMLLIElement;
  readonly id: TunnelId;
  /// The latest view, which the handlers read: they are attached once.
  view: RowView | null;
  readonly light: HTMLSpanElement;
  readonly name: HTMLElement;
  readonly timing: HTMLSpanElement;
  /// Over the card while a green light's evidence has expired.
  readonly expired: HTMLParagraphElement;
  readonly reason: HTMLParagraphElement;
  /// The fault in words, its original text collapsed.
  readonly fault: HTMLDivElement;
  readonly failures: HTMLParagraphElement;
  readonly stats: {
    latency: HTMLElement;
    verified: HTMLElement;
    os: HTMLElement;
    target: HTMLElement;
    observe: HTMLElement;
    /// The observe-target row, left out when there are none.
    observeRow: HTMLElement;
  };
  readonly connect: HTMLButtonElement;
  readonly disconnect: HTMLButtonElement;
  readonly probe: HTMLButtonElement;
  readonly reconnect: HTMLButtonElement;
  readonly monitoring: CardSwitch;
  readonly auto: CardSwitch;
  readonly notify: CardSwitch;
  readonly settings: HTMLDetailsElement;
  readonly settingsNote: HTMLSpanElement;
  readonly form: {
    address: HTMLInputElement;
    port: HTMLInputElement;
    observe: HTMLTextAreaElement;
    targetNow: HTMLElement;
    interval: TimingField;
    timeout: TimingField;
    retries: TimingField;
    minFault: TimingField;
    judgment: HTMLParagraphElement;
    /// The recovery retry (owner decision of 2026-11-04), left out for a
    /// watchdog older than 1.2.0.
    retryFields: HTMLFieldSetElement;
    retryMode: HTMLSelectElement;
    retryLimit: TimingField;
    retryInterval: TimingField;
    save: HTMLButtonElement;
    reset: HTMLButtonElement;
    status: HTMLDivElement;
  };
  readonly external: CardSwitch;
  readonly remove: HTMLButtonElement;
  readonly removeStatus: HTMLDivElement;
  readonly names: HTMLElement[];
  readonly facts: HTMLDListElement;
  readonly observe: HTMLUListElement;
  /// What `facts` and `observe` were last built from.
  factsKey: string;
  observeKey: string;
}

function cardSwitch(key: Key, onChange: (checked: boolean) => void): CardSwitch {
  const { label, box } = switchControl(key, onChange);
  const note = el("p", { className: "note" });
  note.id = uid("note");
  box.setAttribute("aria-describedby", note.id);
  return { label, box, note };
}

function numberInput(range: { readonly min: number; readonly max: number }): HTMLInputElement {
  const input = el("input");
  input.type = "number";
  input.inputMode = "numeric";
  input.min = String(range.min);
  input.max = String(range.max);
  input.step = "1";
  input.required = true;
  return input;
}

function timingField(range: { readonly min: number; readonly max: number }): TimingField {
  return { input: numberInput(range), current: el("span", { className: "current" }) };
}

/// The sr-only tail that names the tunnel in a heading or summary, so every
/// card's settings read apart.
function nameTail(): HTMLSpanElement {
  return el("span", { className: "sr-only" });
}

export function createTunnelCard(id: TunnelId, actions: CardActions): TunnelCard {
  const label = () => card.view?.label ?? id;
  const send = (request: Request) => actions.send(request, label());
  const button = (key: Key, request: Request) => actionButton(key, () => void send(request));

  const monitoring = cardSwitch("card.monitoring", (checked) => {
    void send({ op: "set_tunnel_monitoring", tunnel_id: id, enabled: checked });
  });
  const auto = cardSwitch("card.auto_recovery", (checked) => {
    void send({ op: "set_auto_recovery", tunnel_id: id, enabled: checked });
  });
  const notify = cardSwitch("card.notifications", (checked) => {
    void send({ op: "set_tunnel_notifications", tunnel_id: id, enabled: checked });
  });
  const external = cardSwitch("card.start_when_stopped_externally", (checked) => {
    if (checked) {
      // Only an explicit confirmation turns it on (plan §6.4).
      external.box.checked = false;
      actions.confirmExternal(label(), () => {
        void send({ op: "set_start_when_stopped_externally", tunnel_id: id, enabled: true });
      });
    } else {
      void send({ op: "set_start_when_stopped_externally", tunnel_id: id, enabled: false });
    }
  });

  const address = el("input");
  address.placeholder = "192.0.2.1";
  address.autocomplete = "off";
  address.spellcheck = false;
  const port = el("input");
  labelAttr(port, "placeholder", "card.port_placeholder");
  port.inputMode = "numeric";
  port.autocomplete = "off";
  const retryMode = el("select");
  for (const [value, key] of [
    ["limited", "card.retry_mode_limited"],
    ["forever", "card.retry_mode_forever"],
  ] as const) {
    const option = el("option", { key });
    option.value = value;
    retryMode.append(option);
  }
  const observe = el("textarea");
  observe.rows = 3;
  labelAttr(observe, "placeholder", "card.observe_placeholder");
  observe.spellcheck = false;

  const form = {
    address,
    port,
    observe,
    targetNow: el("span", { className: "current" }),
    interval: timingField(PROBE_INTERVAL_SECS),
    timeout: timingField(PROBE_TIMEOUT_SECS),
    retries: timingField(PROBE_RETRIES),
    minFault: timingField(PROBE_MIN_FAULT_SECS),
    judgment: el("p", { className: "judgment" }),
    retryFields: el("fieldset"),
    retryMode,
    retryLimit: timingField(RECOVERY_RETRY_LIMIT),
    retryInterval: timingField(RECOVERY_RETRY_INTERVAL_SECS),
    save: el("button", { key: "common.save_settings", className: "primary" }),
    reset: actionButton("common.reset", () => fill(card)),
    status: el("div", { className: "form-status" }),
  };
  form.status.setAttribute("role", "status");
  form.save.type = "submit";

  const remove = actionButton("card.remove", () => {
    actions.confirmRemove(label(), () => {
      speak(card.removeStatus, () => plain(t("common.sending")));
      void send({ op: "unregister_tunnel", tunnel_id: id }).then(({ ok, words }) => {
        speak(card.removeStatus, ok ? () => plain(t("card.remove_accepted")) : words);
        if (ok) actions.removed();
      });
    });
  }, "danger");

  // The summaries and the form's groups name the tunnel to assistive
  // technology, so sixteen cards' fields read apart.
  const names = [nameTail(), nameTail(), nameTail(), nameTail(), nameTail()];
  const card: TunnelCard = {
    node: el("li", { className: "card" }),
    id,
    view: null,
    light: el("span", { className: "light" }),
    name: el("h3", { className: "name" }),
    timing: el("span", { className: "timing" }),
    expired: el("p", { className: "expired-note" }),
    reason: el("p", { className: "reason" }),
    fault: el("div", { className: "fault" }),
    failures: el("p", { className: "failures" }),
    stats: {
      latency: el("dd"),
      verified: el("dd"),
      os: el("dd"),
      target: el("dd"),
      observe: el("dd"),
      observeRow: el("div", { className: "wide" }),
    },
    connect: button("card.connect", { op: "connect", tunnel_id: id }),
    disconnect: button("card.disconnect", { op: "disconnect", tunnel_id: id }),
    probe: button("card.probe_now", { op: "probe_now", tunnel_id: id }),
    reconnect: button("card.reconnect", { op: "reconnect", tunnel_id: id }),
    monitoring,
    auto,
    notify,
    settings: el("details", { className: "settings" }),
    settingsNote: el("span", { className: "summary-note" }),
    form,
    external,
    remove,
    removeStatus: el("div", { className: "form-status" }),
    names,
    facts: el("dl", { className: "facts" }),
    observe: el("ul", { className: "observe" }),
    factsKey: "",
    observeKey: "",
  };
  card.light.setAttribute("role", "img");
  card.removeStatus.setAttribute("role", "status");

  const stat = (term: Key, value: HTMLElement, className = "") =>
    el("div", { className }, el("dt", { key: term }), value);
  const timingInputs = [form.interval, form.timeout, form.retries, form.minFault];
  const formNode = el(
    "form",
    { className: "settings-form" },
    el(
      "fieldset",
      {},
      el("legend", {}, el("span", { key: "card.targets_title" }), names[2]!),
      el(
        "div",
        { className: "grid target-grid" },
        field({ key: "card.recovery_target_label" }, address, form.targetNow),
        field({ key: "card.port_label" }, port),
        field(
          { key: "card.observe_label", params: { max: MAX_OBSERVE_TARGETS } },
          observe,
          el("span", { className: "note", key: "card.observe_note" }),
        ),
      ),
      el("p", { className: "hint", key: "card.targets_hint" }),
    ),
    el(
      "fieldset",
      {},
      el("legend", {}, el("span", { key: "card.timing_title" }), names[3]!),
      el(
        "div",
        { className: "grid timing-grid" },
        field({ key: "card.interval_label", params: PROBE_INTERVAL_SECS }, form.interval.input, form.interval.current),
        field({ key: "card.timeout_label", params: PROBE_TIMEOUT_SECS }, form.timeout.input, form.timeout.current),
        field({ key: "card.retries_label", params: PROBE_RETRIES }, form.retries.input, form.retries.current),
        field({ key: "card.min_fault_label", params: PROBE_MIN_FAULT_SECS }, form.minFault.input, form.minFault.current),
      ),
      form.judgment,
    ),
    form.retryFields,
    el("div", { className: "button-row" }, form.save, form.reset),
    form.status,
  );
  form.retryFields.append(
    el("legend", {}, el("span", { key: "card.retry_title" }), names[4]!),
    el(
      "div",
      { className: "grid timing-grid" },
      field({ key: "card.retry_mode_label" }, form.retryMode),
      field({ key: "card.retry_limit_label", params: RECOVERY_RETRY_LIMIT }, form.retryLimit.input, form.retryLimit.current),
      field(
        { key: "card.retry_interval_label", params: RECOVERY_RETRY_INTERVAL_SECS },
        form.retryInterval.input,
        form.retryInterval.current,
      ),
    ),
    el("p", { className: "hint", key: "card.retry_hint" }),
  );
  // A limit means nothing while the tunnel keeps trying.
  form.retryMode.addEventListener("change", () => {
    form.retryLimit.input.disabled = form.retryMode.value === "forever";
  });
  // The judgment time describes every timing field too, so it is heard
  // while one is edited.
  form.judgment.id = `${form.interval.input.id}-judgment`;
  for (const f of timingInputs) {
    f.input.setAttribute("aria-describedby", `${f.input.getAttribute("aria-describedby") ?? ""} ${form.judgment.id}`.trim());
    f.input.addEventListener("input", () => showJudgment(card));
  }
  formNode.addEventListener("submit", (event) => {
    event.preventDefault();
    save(card, send);
  });

  card.settings.append(
    el("summary", {}, el("span", { key: "card.settings" }), names[0]!, card.settingsNote),
    formNode,
    el(
      "section",
      { className: "advanced" },
      el("h4", { key: "card.advanced" }),
      external.label,
      external.note,
      el("p", { className: "hint", key: "card.external_hint" }),
    ),
    el(
      "section",
      { className: "danger-zone" },
      el("h4", { key: "card.remove_title" }),
      el("p", { className: "hint", key: "card.remove_hint" }),
      remove,
      card.removeStatus,
    ),
  );
  card.settings.addEventListener("toggle", () => {
    if (card.settings.open) fill(card);
  });

  const details = el(
    "details",
    { className: "more" },
    el("summary", {}, el("span", { key: "card.details" }), names[1]!),
    card.facts,
    el("h4", { key: "card.observe_title" }),
    card.observe,
  );

  card.stats.observeRow.append(el("dt", { key: "card.observe_stat" }), card.stats.observe);
  card.node.append(
    el("div", { className: "card-head" }, card.light, card.name, card.timing),
    card.expired,
    card.reason,
    card.fault,
    card.failures,
    el(
      "dl",
      { className: "stats" },
      stat("card.latency", card.stats.latency),
      stat("card.last_verified", card.stats.verified),
      stat("card.os_state", card.stats.os),
      stat("card.recovery_target", card.stats.target),
      card.stats.observeRow,
    ),
    // Actions and switches share a row when there is room; the switches'
    // notes go below it, each still describing its own switch.
    el(
      "div",
      { className: "card-controls" },
      el("div", { className: "button-group" }, card.connect, card.disconnect),
      el("div", { className: "button-group" }, card.probe, card.reconnect),
      el("div", { className: "switches" }, monitoring.label, auto.label, notify.label),
    ),
    el("div", { className: "switch-notes" }, monitoring.note, auto.note, notify.note),
    card.settings,
    details,
  );
  return card;
}

/// Fills the settings form with the watchdog's current values.
function fill(card: TunnelCard): void {
  const s: SettingsView | undefined = card.view?.settings;
  if (!s) return;
  const f = card.form;
  f.address.value = s.recoveryAddress;
  f.port.value = s.recoveryPort;
  f.observe.value = s.observeText;
  f.interval.input.value = String(s.probe.interval_secs);
  f.timeout.input.value = String(s.probe.timeout_secs);
  f.retries.input.value = String(s.probe.retries);
  f.minFault.input.value = String(s.probe.min_fault_secs);
  f.retryMode.value = s.retry.limit === null ? "forever" : "limited";
  f.retryLimit.input.value = String(s.retry.limit ?? 100);
  f.retryLimit.input.disabled = s.retry.limit === null;
  f.retryInterval.input.value = String(s.retry.interval_secs);
  speak(f.status, null);
  showJudgment(card);
}

function typedTiming(card: TunnelCard): Parsed<ProbeTiming> {
  const f = card.form;
  return parseTiming({
    interval: f.interval.input.value,
    timeout: f.timeout.input.value,
    retries: f.retries.input.value,
    minFault: f.minFault.input.value,
  });
}

function typedRetry(card: TunnelCard): Parsed<RecoveryRetry> {
  const f = card.form;
  return parseRetry({
    forever: f.retryMode.value === "forever",
    limit: f.retryLimit.input.value,
    interval: f.retryInterval.input.value,
  });
}

/// The judgment time of the values being typed, live, or what is wrong
/// with them; said again from the fields as they are after a language
/// change.
function showJudgment(card: TunnelCard): void {
  const timing = typedTiming(card);
  speak(card.form.judgment, () => {
    const now = typedTiming(card);
    return plain(now.ok ? judgmentText(now.value) : now.error);
  });
  card.form.judgment.classList.toggle("invalid", !timing.ok);
}

function save(card: TunnelCard, send: (request: Request) => Promise<{ ok: boolean; words: Words }>): void {
  const f = card.form;
  // What is wrong is worded again in another language from the same parse.
  const refuse = (text: () => string) => speak(f.status, () => plain(text()));
  if (unavailable(f.save)) return refuse(() => f.save.title);
  let recovery: ProbeTarget | null = null;
  if (f.address.value.trim() !== "") {
    const parsed = parseTarget(f.address.value, f.port.value);
    if (!parsed.ok) return refuse(() => t("card.target_error", { error: parsed.error }));
    recovery = parsed.value;
  } else if (f.port.value.trim() !== "") {
    return refuse(() => t("card.port_without_target"));
  }
  const observe = parseObserveTargets(f.observe.value);
  if (!observe.ok) return refuse(() => observe.error);
  const timing = typedTiming(card);
  if (!timing.ok) return refuse(() => timing.error);
  // The retry goes only to a watchdog that has it, and only when it changed.
  const settings = card.view?.settings;
  const supported = settings?.retrySupported === true;
  const retry = supported ? typedRetry(card) : null;
  if (retry !== null && !retry.ok) return refuse(() => retry.error);
  const changedRetry =
    retry !== null && retry.ok && settings !== undefined && JSON.stringify(retry.value) !== JSON.stringify(settings.retry)
      ? retry.value
      : null;
  refuse(() => t("common.sending"));
  void send({
    op: "update_monitor_settings",
    tunnel_id: card.id,
    settings: { recovery_target: recovery, observe_targets: observe.value, probe: timing.value },
  })
    .then((first) =>
      first.ok && changedRetry !== null
        ? send({ op: "set_recovery_retry", tunnel_id: card.id, retry: changedRetry })
        : first,
    )
    .then(({ ok, words }) => speak(f.status, ok ? () => plain(t("common.accepted")) : words));
}

function updateSwitch(s: CardSwitch, view: SwitchView, name: string): void {
  if (s.box.checked !== view.on) s.box.checked = view.on;
  setControl(s.box, view.toggle, name);
  setText(s.note, view.note);
  setHidden(s.note, view.note === "");
}

export function updateTunnelCard(card: TunnelCard, view: RowView, actions: CardActions): void {
  card.view = view;
  const name = view.label;
  setAttr(
    card.node,
    "class",
    `card light-${view.light}${view.intended ? " intended" : ""}${view.monitoring.on ? "" : " paused"}`,
  );
  setText(card.light, `${view.icon} ${view.lightLabel}`);
  setAttr(card.light, "aria-label", t("common.labelled", { label: view.lightLabel, text: view.reason }));
  setText(card.expired, view.expired ?? "");
  setHidden(card.expired, view.expired === null);
  setText(card.name, name);
  setText(card.reason, view.reason);
  showSaid(card.fault, view.fault);
  setHidden(card.fault, view.fault === null);
  setText(card.failures, view.failures ?? "");
  setHidden(card.failures, view.failures === null);

  const [kind, at] =
    view.deadlineMs !== null
      ? ["deadline", view.deadlineMs]
      : view.nextRetryMs !== null
        ? ["retry", view.nextRetryMs]
        : [null, null];
  setAttr(card.timing, "data-kind", kind);
  setAttr(card.timing, "data-at", at === null ? null : String(at));
  setText(card.timing, kind !== null && at !== null ? actions.timingText(kind, at) : "");

  setText(card.stats.latency, view.latency);
  setText(
    card.stats.verified,
    view.lastVerifiedMs === null ? "—" : actions.time(view.lastVerifiedMs, view.updatedMs),
  );
  setText(card.stats.os, view.osState);
  setText(card.stats.target, view.target);
  setText(card.stats.observe, view.observeShort);
  setHidden(card.stats.observeRow, view.observe.length === 0);

  // Every control is named after the tunnel it acts on.
  const named = (key: Key) => t("common.labelled", { label: t(key), text: name });
  setControl(card.connect, view.connect, named("card.connect"));
  setControl(card.disconnect, view.disconnect, named("card.disconnect"));
  setControl(card.probe, view.probeNow, named("card.probe_now"));
  setControl(card.reconnect, view.reconnect, named("card.reconnect"));
  updateSwitch(card.monitoring, view.monitoring, named("card.monitoring"));
  updateSwitch(card.auto, view.autoRecovery, named("card.auto_recovery"));
  updateSwitch(card.notify, view.notifications, named("card.notifications"));
  updateSwitch(card.external, view.startWhenStoppedExternally, named("card.start_when_stopped_externally"));
  setControl(card.form.save, view.edit, named("common.save_settings"));
  setControl(card.remove, view.unregister, named("card.remove_title"));
  for (const tail of card.names) setText(tail, t("card.name_tail", { name }));

  // The current values next to the fields; the fields themselves are left
  // to the user.
  const p = view.settings.probe;
  setText(card.settingsNote, view.judgmentShort);
  setText(card.form.targetNow, t("common.current", { value: view.target }));
  setText(card.form.interval.current, t("card.current_secs", { secs: p.interval_secs }));
  setText(card.form.timeout.current, t("card.current_secs", { secs: p.timeout_secs }));
  setText(card.form.retries.current, t("card.current_retries", { retries: p.retries, needed: p.retries + 1 }));
  setText(card.form.minFault.current, t("card.current_secs", { secs: p.min_fault_secs }));
  const r = view.settings.retry;
  setHidden(card.form.retryFields, !view.settings.retrySupported);
  setText(
    card.form.retryLimit.current,
    r.limit === null ? t("card.current_retry_forever") : t("card.current_retry_limit", { limit: r.limit }),
  );
  setText(card.form.retryInterval.current, t("card.current_secs", { secs: r.interval_secs }));

  // Rebuilt only when their content changed: nothing in them takes focus,
  // but a selection in them would be lost.
  const factsKey = JSON.stringify(view.details);
  if (factsKey !== card.factsKey) {
    card.factsKey = factsKey;
    card.facts.replaceChildren(
      ...view.details.map(([k, v]) => el("div", {}, el("dt", { text: k }), el("dd", { text: v }))),
    );
  }
  const observe = view.observe.map((o) => {
    const time = o.atMs === null ? "" : actions.time(o.atMs, view.updatedMs);
    const when =
      o.atMs === null
        ? ""
        : o.note === ""
          ? t("card.observe_when", { time })
          : t("card.observe_when_note", { time, note: o.note });
    // Only a current failure is marked as one.
    return { failing: o.failing && o.current, text: t("card.observe_item", { label: o.label, result: o.result, when }) };
  });
  const observeKey = JSON.stringify(observe);
  if (observeKey !== card.observeKey) {
    card.observeKey = observeKey;
    card.observe.replaceChildren(
      ...(observe.length > 0
        ? observe.map((o) => el("li", { className: o.failing ? "failing" : "", text: o.text }))
        : [el("li", { key: "common.none" })]),
    );
  }
}
