// Keeps one node per key across renders. The window shows up to 16 tunnels
// and 32 discovered ones and redraws them on every heartbeat (5 s); a node
// that left the document and came back would drop keyboard focus, and a
// rebuilt list would also lose the scroll position and every open
// "details". So a render only updates the nodes it already has, creates the
// ones for new keys, removes the ones whose key is gone and moves a node
// only when the order changed. No DOM types here: the container is anything
// that inserts and removes children, which keeps this testable without a
// browser.

/// The container's side: `insertBefore(node, null)` appends, and inserting a
/// node that is already a child moves it.
export interface Container<N> {
  insertBefore(node: N, child: N | null): unknown;
  removeChild(node: N): unknown;
}

export class KeyedList<K, V, N, T extends { readonly node: N }> {
  private readonly items = new Map<K, T>();
  private order: K[] = [];

  constructor(
    private readonly parent: Container<N>,
    private readonly create: (key: K) => T,
    private readonly update: (item: T, value: V) => void,
  ) {}

  /// Shows `values` in their order. The container's children are exactly
  /// this list's nodes. A key given twice is shown once, at its first place.
  render(values: ReadonlyArray<readonly [K, V]>): void {
    const wanted: Array<readonly [K, V]> = [];
    const keys = new Set<K>();
    for (const entry of values) {
      if (!keys.has(entry[0])) {
        keys.add(entry[0]);
        wanted.push(entry);
      }
    }
    for (const [key, item] of this.items) {
      if (!keys.has(key)) {
        this.parent.removeChild(item.node);
        this.items.delete(key);
      }
    }
    const shown = this.order.filter((key) => keys.has(key));
    wanted.forEach(([key, value], i) => {
      let item = this.items.get(key);
      if (item === undefined) {
        item = this.create(key);
        this.items.set(key, item);
      }
      this.update(item, value);
      if (shown[i] !== key) {
        // Places 0..i-1 already hold earlier keys, so `key` is either new or
        // further down; it moves up to place i.
        const here = shown[i];
        const before = here === undefined ? null : (this.items.get(here)?.node ?? null);
        this.parent.insertBefore(item.node, before);
        const old = shown.indexOf(key);
        if (old !== -1) shown.splice(old, 1);
        shown.splice(i, 0, key);
      }
    });
    this.order = shown;
  }

  /// The item shown for `key`, if any.
  get(key: K): T | undefined {
    return this.items.get(key);
  }

  get size(): number {
    return this.items.size;
  }
}
