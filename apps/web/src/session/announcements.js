// @ts-check
/** @param {string} rejection */
export function generationRejectionText(rejection) {
  return `Plugin update failed; previous plugins remain in use. ${rejection}`;
}
// One surface's recent identities survive reconnect gaps and window changes.
export class ConversationAnnouncements {
  /** @type {string | undefined} */
  session;
  /** @type {Set<string>} */
  seen = new Set();
  revision = 0;
  /** @type {import('../presentation-types.ts').NativeSurface['transcript']['turns']} */
  turns;
  /** @type {import('../presentation-types.ts').NativeSurface['pending']} */
  pending;
  selected = false;
  /** @type {string | undefined} */
  rejection;
  /** @type {string | undefined} */
  announcedRejection;
  /** @param {import('../presentation-types.ts').NativeSurface | null | undefined} data
   * @param {boolean} selected */
  update(data, selected) {
    if (!data || data.historical) return undefined;
    const initial = this.session !== data.session;
    const rejection = data.projections?.generation_rejection ?? undefined;
    const clearedRejection = !!this.rejection && !rejection;
    if (initial || !rejection) this.announcedRejection = undefined;
    const newRejection = rejection && rejection !== this.announcedRejection;
    if (initial) this.seen.clear();
    if (!initial && this.turns === data.transcript.turns && this.pending === data.pending && this.selected === selected && this.rejection === rejection) return undefined;
    this.turns = data.transcript.turns;
    this.pending = data.pending;
    this.selected = selected;
    this.rejection = rejection;
    const events = new Map();
    for (const turn of Object.values(data.transcript.turns?.entries ?? {})) {
      if (turn.terminal) events.set(`turn:${turn.id}:${turn.terminal}`, `Response ${turn.status.replaceAll('_', ' ').toLowerCase()}.`);
    }
    if (selected) {
      for (const item of data.pending ?? []) events.set(`pending:${item.kind}:${item.owner}:${item.id}`, `${item.title.replace(/[.!?。！？\s]+$/u, '')}. Your attention is needed.`);
    }
    this.session = data.session;
    const fresh = [...events].filter(([key]) => !this.seen.has(key));
    for (const key of events.keys()) {
      this.seen.delete(key);
      this.seen.add(key);
    }
    while (this.seen.size > 4096) {
      const oldest = this.seen.values().next();
      if (!oldest.done) this.seen.delete(oldest.value);
    }
    if (!selected) return undefined;
    this.announcedRejection = rejection;
    const messages = initial ? [] : fresh.map(([, text]) => text);
    if (newRejection) messages.push(generationRejectionText(rejection));
    if (!messages.length) return clearedRejection ? null : undefined;
    return {revision: ++this.revision, text: messages.join(' ')};
  }
}
