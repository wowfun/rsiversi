// One observer and current visible set per transcript; only rendering updates the order.
export class TranscriptVisibility {
  constructor(root, changed, observe = callback => new IntersectionObserver(callback, {root})) {
    this.changed = changed;
    this.observe = observe;
    this.records = new Map();
    this.visible = new Set();
    this.pending = new Set();
  }
  get ready() { return this.pending.size === 0; }
  update(entries, generation) {
    if (this.disposed) return;
    if (!this.observer || generation !== this.generation) {
      this.observer?.disconnect(); this.records.clear(); this.visible.clear(); this.pending.clear();
      this.generation = generation;
      const observer = this.observe(changes => {
        if (this.observer !== observer) return;
        for (const change of changes) {
          if (!this.records.has(change.target)) continue;
          this.pending.delete(change.target);
          if (change.intersectionRect.height > 0) this.visible.add(change.target);
          else this.visible.delete(change.target);
        }
        if (this.ready) this.changed();
      });
      this.observer = observer;
    }
    const retained = new Set();
    let order = 0;
    for (const [key, entry] of entries) {
      retained.add(entry.node);
      const fresh = !this.records.has(entry.node);
      this.records.set(entry.node, {key, entry, order: order++});
      if (fresh) { this.pending.add(entry.node); this.observer.observe(entry.node); }
    }
    for (const node of this.records.keys()) if (!retained.has(node)) {
      this.observer.unobserve(node); this.records.delete(node); this.visible.delete(node); this.pending.delete(node);
    }
    if (this.ready) this.changed();
  }
  last(count) {
    return [...this.visible].map(node => this.records.get(node))
      .filter(record => record && !record.entry.node.hidden)
      .sort((a,b) => a.order-b.order).slice(-count).map(({key,entry}) => [key,entry]);
  }
  dispose() {
    this.disposed = true; this.observer?.disconnect(); this.observer = undefined;
    this.records.clear(); this.visible.clear(); this.pending.clear();
  }
}
