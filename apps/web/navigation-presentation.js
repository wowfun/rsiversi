const formatter = new Intl.DateTimeFormat('en', {month:'short', day:'numeric', hour:'2-digit', minute:'2-digit', hour12:false});
const common = (a, b) => { let i=0; while(i<a.length && i<b.length && a[i]===b[i])i++; return i; };
// One bounded view pass; duplicate rows from pinned/group pages share an identity.
export function navigationTitles(entries) {
  const titles=new Map(), times=new Map();
  for(const entry of entries) {
    if(entry.blank)continue;
    if(entry.metadata.title){titles.set(entry.session,entry.metadata.title);continue;}
    let ids=times.get(entry.created_at_ms);
    if(!ids){ids=new Set();times.set(entry.created_at_ms,ids);}
    ids.add(entry.session);
  }
  for(const [time,ids] of times) {
    const date=new Date(Number(time));
    const stamp=Number.isFinite(date.getTime())?formatter.format(date):`Created ${time}`;
    const sorted=[...ids].sort();
    for(let i=0;i<sorted.length;i++) {
      const id=sorted[i];
      const width=Math.max(8,common(id,sorted[i-1]??'')+1,common(id,sorted[i+1]??'')+1);
      titles.set(id,`${stamp} · ${id.slice(0,width)}`);
    }
  }
  return titles;
}
export function navigationAttention(observation) {
  const attention=new Map();
  for(const row of observation?.entries??[]) {
    const {kind,id}=row.position.conversation;
    if(kind==='native')attention.set(id,row);
  }
  return attention;
}
export class GroupRestoration {
  pending=new Set();
  async run(key, load) {
    if(this.pending.has(key) || this.pending.size>=16)return;
    this.pending.add(key);
    try { await load(); } finally { this.pending.delete(key); }
  }
}

export function conversationRecency(timestamp, now=Date.now()) {
  const value=Number(timestamp);
  if(!Number.isFinite(value)||value<0)return '';
  const seconds=Math.max(0,Math.floor((now-value)/1000));
  if(seconds<60)return 'now';
  if(seconds<3600)return `${Math.floor(seconds/60)}min`;
  if(seconds<86400)return `${Math.floor(seconds/3600)}h`;
  return `${Math.floor(seconds/86400)}d`;
}

/** @template {{session: string, blank?: boolean}} T
 * @param {T[]} entries
 * @param {string | undefined} selected
 */
export function sidebarConversations(entries, selected) {
  if (!entries.some(entry => entry.blank)) return entries;
  const started = entries.filter(entry => !entry.blank);
  const current = selected && entries.find(entry => entry.blank && entry.session === selected);
  if (current) started.unshift(current);
  return started;
}

/** sidebarConversations has already placed the optional current blank first.
 * @param {string} [reveal] */
export function workspaceRowWindow(entries, selected, attention, stored, reveal) {
  const first = entries[0]?.blank ? 1 : 0;
  const count = entries.length - first;
  let minimum = 5;
  for (let index = first + minimum; index < Math.min(entries.length, first + 50); index++) {
    const id = entries[index].session;
    const status = attention.get(id)?.status;
    if (id === selected || status === 'running' || status === 'waiting' || status === 'unread') {
      minimum = index - first + 1;
    }
  }
  if (reveal !== undefined) {
    const index = entries.findIndex(entry => entry.session === reveal);
    minimum = Math.max(minimum, index - first + 1);
  }
  const limit = Math.max(stored, minimum);
  return {minimum, limit, visible: first + Math.min(count, limit), canFold: count > minimum};
}

export function sessionWorkspaceGroup(surface, navigation) {
  if (surface?.kind !== 'native') return;
  const find = id => {
    const entry = navigation?.entries.find(row => row.session === id);
    if (entry) return entry.workspace ?? 'other';
    const pin = navigation?.pins.find(row => row.status === 'available' && row.entry.session === id);
    if (pin) return pin.entry.workspace ?? 'other';
    for (const group of Object.values(navigation?.groups ?? {})) {
      if (group.entries.some(row => row.session === id))
        return group.workspace.id ?? 'other';
    }
  };
  const saved = find(surface.session);
  if (saved !== undefined || !surface.draft || !surface.creation) return saved;
  const {workspace_id, fork} = surface.creation;
  if (fork) return find(fork.source_session_id) === workspace_id ? workspace_id : undefined;
  return workspace_id;
}
