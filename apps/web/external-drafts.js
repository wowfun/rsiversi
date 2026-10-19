// Connection-owned, bounded external drafts. A receipt belongs to one record revision.
// Retention and revision policy: apps/external-drafts.md.
const MAXIMUM_DRAFTS = 16, MAXIMUM_BYTES = 1024 * 1024;
export function externalDrafts() {
  const records = new Map(), encoder = new TextEncoder();
  let epoch = {}, clock = 0;
  const weight = record => record.bytes + (record.pending && record.pending.revision !== record.revision ? record.pending.bytes : 0);
  const current = record => !!record && records.get(record.id) === record;
  const notify = record => { for (const changed of record.bindings) changed(); };
  function admit(record, extra, count = 0) {
    let size = records.size + count, total = extra;
    for (const value of records.values()) total += weight(value);
    if (size <= MAXIMUM_DRAFTS && total <= MAXIMUM_BYTES) return true;
    const victims = [...records.values()].filter(value => value !== record && !value.pending && !value.bindings.size).sort((a,b) => a.used-b.used);
    const evicted = [];
    while (size > MAXIMUM_DRAFTS || total > MAXIMUM_BYTES) {
      const victim = victims.shift();
      if (!victim) return false;
      if (size <= MAXIMUM_DRAFTS && weight(victim) === 0) continue;
      evicted.push(victim); size--; total -= weight(victim);
    }
    for (const victim of evicted) records.delete(victim.id);
    return true;
  }
  return {
    bind(id, changed) {
      let record = records.get(id);
      if (!record) {
        if (!admit(null, 0, 1)) return null;
        record = {id, text:'', bytes:0, revision:0, unknownRevision:null, pending:null, bindings:new Set(), used:0};
        records.set(id, record);
      }
      record.used = ++clock; record.bindings.add(changed);
      return record;
    },
    unbind(record, changed) { if(record?.bindings.delete(changed)) {clock++;if(!record.bindings.size)record.used=clock;} },
    edit(record, text) {
      if (!record || !current(record)) return false;
      if (record.text === text) return true;
      const bytes = encoder.encode(text).length;
      const next = bytes + (record.pending ? record.pending.bytes : 0);
      if (!admit(record, next-weight(record))) return false;
      record.text = text; record.bytes = bytes; record.revision++; record.unknownRevision = null; record.used = ++clock;
      notify(record); return true;
    },
    begin(record) {
      if (!record || !current(record) || record.pending || record.unknownRevision === record.revision) return null;
      const attempt = {epoch, record, revision:record.revision, text:record.text, bytes:record.bytes};
      record.pending = attempt; record.used = ++clock; notify(record);
      return attempt;
    },
    finish(attempt, succeeded) {
      const record = attempt.record;
      if (attempt.epoch !== epoch || !current(record) || record.pending !== attempt) return;
      record.pending = null;
      if (record.revision === attempt.revision) {
        if (succeeded) { record.text = ''; record.bytes = 0; record.revision++; record.unknownRevision = null; }
        else record.unknownRevision = attempt.revision;
      }
      record.used = ++clock; notify(record);
    },
    clear() {
      // Fence old completions before callbacks can bind records for the new connection.
      epoch = {};
      clock++;
      const previous = [...records.values()]; records.clear();
      for (const record of previous) { record.text = ''; record.bytes = 0; record.pending = null; record.unknownRevision = null; notify(record); record.bindings.clear(); }
    },
    current,
    version: () => clock,
  };
}
