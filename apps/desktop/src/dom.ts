// Small DOM helpers shared by the tabs. Text from the watchdog (names,
// reasons, error details) is only ever set with `textContent`, never parsed
// as HTML, and nothing is styled inline (the CSP allows only the packaged
// stylesheet).
//
// Controls that cannot be used are marked `aria-disabled` instead of
// `disabled`: they stay focusable, so focus is not lost when a control turns
// unavailable under it (a refresh after a manual reconnect, the watchdog
// going lost),
// and the reason in `title` reaches keyboard and screen reader users as the
// control's description. Their handlers do nothing while marked.
//
// Fixed words (labels, headings, hints) are marked with the key of their
// text (`data-i18n`, `data-i18n-<attribute>`), in `index.html` and on what
// `el` builds, so `relabel` can put the whole page into another language
// in place, without rebuilding anything or touching what the user typed.

import { type Key, type Params, t } from "./i18n";
import type { Said, Words } from "./said";
import type { Availability } from "./view";

/// An element; `key` (with `params`) gives it a fixed text from the
/// language file that `relabel` keeps in the current language.
export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: { className?: string; text?: string; key?: Key; params?: Params } = {},
  ...children: Array<Node | string>
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (props.className) node.className = props.className;
  if (props.key !== undefined) {
    node.dataset.i18n = props.key;
    if (props.params !== undefined) node.dataset.i18nParams = JSON.stringify(props.params);
    node.textContent = t(props.key, props.params);
  } else if (props.text !== undefined) {
    node.textContent = props.text;
  }
  for (const child of children) node.append(child);
  return node;
}

/// The attributes `relabel` fills from the language file.
const LABELLED_ATTRS = ["placeholder", "aria-label", "label", "title"] as const;
type LabelledAttr = (typeof LABELLED_ATTRS)[number];

const dataName = (attr: LabelledAttr) => `i18n${attr.replace(/(^|-)([a-z])/g, (_, __, c: string) => c.toUpperCase())}`;

/// Gives `node` a fixed attribute text from the language file, kept in the
/// current language by `relabel`.
export function labelAttr(node: HTMLElement, attr: LabelledAttr, key: Key): void {
  node.dataset[dataName(attr)] = key;
  node.setAttribute(attr, t(key));
}

function paramsOf(node: HTMLElement): Params | undefined {
  const text = node.dataset.i18nParams;
  if (text === undefined) return undefined;
  try {
    return JSON.parse(text) as Params;
  } catch {
    return undefined;
  }
}

/// Puts every fixed text under `root` into the current language: elements
/// marked `data-i18n` get their text, `data-i18n-placeholder` and the like
/// their attribute. Only marked text changes; inputs keep what was typed.
export function relabel(root: ParentNode = document): void {
  for (const node of root.querySelectorAll<HTMLElement>("[data-i18n]")) {
    setText(node, t(node.dataset.i18n as Key, paramsOf(node)));
  }
  for (const attr of LABELLED_ATTRS) {
    for (const node of root.querySelectorAll<HTMLElement>(`[data-i18n-${attr}]`)) {
      setAttr(node, attr, t(node.dataset[dataName(attr)] as Key));
    }
  }
}

/// Status lines whose words came from an event (an answer, a typed value)
/// rather than a render, with the words to say there: `respeak` says them
/// again after a language change. A line that left the page is forgotten.
const spoken = new Map<HTMLElement, Words>();

/// Shows `said` in `node`: the sentence, and the original text, when there
/// is one, inside a collapsed details element. Rebuilt only when it says
/// something new, so an opened details stays open and a status region is
/// not announced again.
export function showSaid(node: HTMLElement, said: Said | null): void {
  const key = said === null ? "" : JSON.stringify([said.text, said.detail]);
  if ((node.dataset.said ?? "") === key) return;
  if (key === "") delete node.dataset.said;
  else node.dataset.said = key;
  if (said === null) {
    node.replaceChildren();
    return;
  }
  const children: Node[] = [document.createTextNode(said.text)];
  if (said.detail !== null) {
    children.push(
      el("details", { className: "raw" }, el("summary", { key: "errors.raw_title" }), el("code", { text: said.detail })),
    );
  }
  node.replaceChildren(...children);
}

/// Says `words` in `node` now, and again in the new language after a
/// language change; null clears the line.
export function speak(node: HTMLElement, words: Words | null): void {
  if (words === null) spoken.delete(node);
  else spoken.set(node, words);
  showSaid(node, words === null ? null : words());
}

/// Says every kept status line again, in the current language.
export function respeak(): void {
  for (const [node, words] of spoken) {
    if (node.isConnected) showSaid(node, words());
    else spoken.delete(node);
  }
}

export function byId(id: string): HTMLElement {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing #${id}`);
  return node;
}

/// Sets text only when it changed: an unchanged value keeps its text node
/// (and any selection in it), and a status or alert region is not announced
/// again for the same text.
export function setText(node: Node, text: string): void {
  if (node.textContent !== text) node.textContent = text;
}

export function setAttr(node: Element, name: string, value: string | null): void {
  if (value === null) {
    if (node.hasAttribute(name)) node.removeAttribute(name);
  } else if (node.getAttribute(name) !== value) {
    node.setAttribute(name, value);
  }
}

export function setHidden(node: HTMLElement, hidden: boolean): void {
  if (node.hidden !== hidden) node.hidden = hidden;
}

/// Marks a control usable or not, with the reason as its description, and
/// names it after the tunnel it acts on when `name` is given: sixteen
/// connect buttons are told apart by their accessible names.
export function setControl(control: HTMLElement, availability: Availability, name?: string): void {
  setAttr(control, "aria-disabled", availability.kind === "disabled" ? "true" : null);
  setAttr(control, "title", availability.kind === "disabled" ? availability.why : null);
  if (name !== undefined) setAttr(control, "aria-label", name);
}

export function unavailable(control: Element): boolean {
  return control.getAttribute("aria-disabled") === "true";
}

/// A button that acts only while it is not marked unavailable, labelled
/// with a fixed text (`key`) or none (set later).
export function actionButton(key: Key | null, onClick: () => void, className?: string): HTMLButtonElement {
  const b = key === null ? el("button", { className }) : el("button", { key, className });
  b.type = "button";
  b.addEventListener("click", () => {
    if (!unavailable(b)) onClick();
  });
  return b;
}

/// A switch: a checkbox drawn as a switch, with its words beside it, that
/// cannot be flipped while marked unavailable. The box shows the watchdog's
/// state; `onChange` asks for the other one.
export function switchControl(
  key: Key,
  onChange: (checked: boolean) => void,
): { label: HTMLLabelElement; box: HTMLInputElement } {
  const box = el("input");
  box.type = "checkbox";
  box.setAttribute("role", "switch");
  box.addEventListener("click", (event) => {
    if (unavailable(box)) event.preventDefault();
  });
  box.addEventListener("change", () => onChange(box.checked));
  const label = el("label", { className: "switch" }, box, el("span", { key }));
  return { label, box };
}

let ids = 0;

/// A document-unique ID for wiring labels and descriptions.
export function uid(prefix: string): string {
  ids += 1;
  return `${prefix}-${ids}`;
}

/// A labelled field: the label above (a fixed text, `key` with `params`),
/// the control, then any notes, which also describe the control to
/// assistive technology.
export function field(
  label: { key: Key; params?: Params },
  control: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement,
  ...notes: HTMLElement[]
): HTMLDivElement {
  control.id ||= uid("f");
  const node = el("label", label);
  node.htmlFor = control.id;
  for (const note of notes) note.id ||= uid("n");
  if (notes.length > 0) control.setAttribute("aria-describedby", notes.map((n) => n.id).join(" "));
  return el("div", { className: "field" }, node, control, ...notes);
}
