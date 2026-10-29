// Bounded unsaved device intents; storage still owns validation and atomic merge.
export class LayoutEdits {
  entries = new Map();
  add(intent) {
    const put = (key, value) => { this.entries.delete(key); this.entries.set(key, value); };
    if (intent.kind === 'reset') { this.entries.clear(); put('reset', intent); }
    else if (intent.kind === 'patch') {
      for (const [key, value] of Object.entries(intent.patch)) put(`field:${key}`, {kind:'patch',patch:{[key]:value}});
    } else {
      put(`workspace:${intent.id}`, intent);
      const workspaces = [...this.entries.keys()].filter(key=>key.startsWith('workspace:'));
      if (workspaces.length > 16) this.entries.delete(workspaces[0]);
    }
  }
  get size() { return this.entries.size; }
  first() { return this.entries.entries().next().value; }
  saved(key, intent) { if (this.entries.get(key) === intent) this.entries.delete(key); }
}
