const formatter = new Intl.DateTimeFormat('en', {month:'short', day:'numeric', hour:'2-digit', minute:'2-digit', hour12:false});
const common = (a, b) => { let i=0; while(i<a.length && i<b.length && a[i]===b[i])i++; return i; };
// One bounded view pass; duplicate rows from pinned/group pages share an identity.
export function navigationPresentation(entries, observation) {
  const titles=new Map(), attention=new Map(), times=new Map();
  for(const entry of entries) {
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
  for(const row of observation?.entries??[]) {
    const {kind,id}=row.position.conversation;
    if(kind==='native')attention.set(id,row.status);
  }
  return {titles,attention};
}
export class GroupRestoration {
  pending=new Set();
  async run(key, load) {
    if(this.pending.has(key) || this.pending.size>=16)return;
    this.pending.add(key);
    try { await load(); } finally { this.pending.delete(key); }
  }
}
