// The keyed list against a stand-in container that records every insert and
// removal: a heartbeat must not take nodes out of the document, which is
// what would drop keyboard focus, the scroll position and open details.

import { describe, expect, it } from "vitest";

import { KeyedList } from "./keyed";

interface FakeNode {
  readonly key: string;
  value: number;
}

class FakeContainer {
  children: FakeNode[] = [];
  inserts = 0;
  removes = 0;

  insertBefore(node: FakeNode, child: FakeNode | null): void {
    this.inserts += 1;
    const at = this.children.indexOf(node);
    if (at !== -1) this.children.splice(at, 1);
    const before = child === null ? -1 : this.children.indexOf(child);
    if (before === -1) this.children.push(node);
    else this.children.splice(before, 0, node);
  }

  removeChild(node: FakeNode): void {
    this.removes += 1;
    this.children.splice(this.children.indexOf(node), 1);
  }
}

function setup() {
  const parent = new FakeContainer();
  let created = 0;
  let updated = 0;
  const list = new KeyedList<string, number, FakeNode, { node: FakeNode }>(
    parent,
    (key) => {
      created += 1;
      return { node: { key, value: -1 } };
    },
    (item, value) => {
      updated += 1;
      item.node.value = value;
    },
  );
  const keys = () => parent.children.map((n) => n.key);
  return { parent, list, keys, counts: () => ({ created, updated }) };
}

const rows = (keys: string[], value = 0) => keys.map((k) => [k, value] as const);
const sixteen = Array.from({ length: 16 }, (_, i) => `t${i}`);

describe("keyed list", () => {
  it("builds 16 rows once and only updates them on later heartbeats", () => {
    const { parent, list, keys, counts } = setup();
    list.render(rows(sixteen));
    expect(keys()).toEqual(sixteen);
    expect(parent.inserts).toBe(16);
    const first = [...parent.children];
    // Two days of heartbeats every 5 s would be 34,560; a thousand shows it.
    for (let beat = 1; beat <= 1_000; beat += 1) list.render(rows(sixteen, beat));
    expect(parent.inserts).toBe(16);
    expect(parent.removes).toBe(0);
    expect(counts().created).toBe(16);
    expect(counts().updated).toBe(16 * 1_001);
    expect(parent.children.every((node, i) => node === first[i])).toBe(true);
    expect(parent.children.every((node) => node.value === 1_000)).toBe(true);
    expect(list.size).toBe(16);
  });

  it("removes only the row that left and adds only the new one", () => {
    const { parent, list, keys } = setup();
    list.render(rows(sixteen));
    const kept = parent.children.filter((n) => n.key !== "t5");
    list.render(rows(sixteen.filter((k) => k !== "t5")));
    expect(parent.removes).toBe(1);
    expect(parent.inserts).toBe(16);
    expect(keys()).toEqual(sixteen.filter((k) => k !== "t5"));
    expect(parent.children.every((node, i) => node === kept[i])).toBe(true);
    list.render(rows([...sixteen.filter((k) => k !== "t5"), "t16"]));
    expect(parent.inserts).toBe(17);
    expect(list.size).toBe(16);
  });

  it("moves only what changed place", () => {
    const { parent, list, keys } = setup();
    list.render(rows(sixteen));
    const order = [...sixteen];
    const [moved] = order.splice(12, 1);
    order.unshift(moved!);
    list.render(rows(order));
    expect(keys()).toEqual(order);
    expect(parent.inserts).toBe(17);
    list.render(rows([...sixteen].reverse()));
    expect(keys()).toEqual([...sixteen].reverse());
    expect(parent.removes).toBe(0);
  });

  it("empties, refills, and shows a repeated key once", () => {
    const { parent, list, keys, counts } = setup();
    list.render(rows(["a", "b"]));
    list.render([]);
    expect(parent.children).toEqual([]);
    expect(list.size).toBe(0);
    list.render([["a", 1], ["b", 2], ["a", 3]]);
    expect(keys()).toEqual(["a", "b"]);
    expect(parent.children[0]?.value).toBe(1);
    expect(counts().created).toBe(4);
  });
});
