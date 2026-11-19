// One tunnel found by discovery, offered for registration. Built once per
// tunnel and updated in place (see `keyed.ts`), like the cards.

import { actionButton, el, setControl, setText, showSaid } from "./dom";
import { t } from "./i18n";
import type { TunnelId } from "./protocol";
import type { DiscoveredView } from "./view";

export interface DiscoveredRow {
  readonly node: HTMLLIElement;
  readonly label: HTMLSpanElement;
  readonly hint: HTMLSpanElement;
  readonly register: HTMLButtonElement;
  readonly status: HTMLDivElement;
  view: DiscoveredView | null;
}

export function createDiscoveredRow(
  id: TunnelId,
  onRegister: (id: TunnelId, label: string) => void,
): DiscoveredRow {
  const row: DiscoveredRow = {
    node: el("li"),
    label: el("span", { className: "label" }),
    hint: el("span", { className: "hint" }),
    register: actionButton(null, () => onRegister(id, row.view?.label ?? id)),
    status: el("div", { className: "status" }),
    view: null,
  };
  // Not a live region of its own: the answer is also said once in the
  // window's message, with the tunnel's name.
  row.node.append(row.label, row.hint, row.register, row.status);
  return row;
}

export function updateDiscoveredRow(row: DiscoveredRow, view: DiscoveredView): void {
  row.view = view;
  setText(row.label, view.label);
  setText(row.hint, view.unsupported ? t("discovered.unsupported_hint") : "");
  setText(row.register, view.action);
  setControl(row.register, view.register, t("discovered.action_label", { action: view.action, label: view.label }));
  showSaid(row.status, view.status.text === "" ? null : view.status);
}
