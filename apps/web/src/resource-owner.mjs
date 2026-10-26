// Ephemeral resource responsibilities belong to the original connection.
let sequence = 0;
const identity = value => `${value.view}:${value.open_revision}`;
const validIdentity = value => value && /^[1-9][0-9]*$/.test(value.view) && /^[1-9][0-9]*$/.test(value.open_revision);
export class ResourceOwner {
  id = ++sequence;
  opens = new Map();
  cleanup = new Map();
  listeners = new Set();
  snapshot = [];
  visible = new Set();
  retired = false;
  constructor(call, mint = () => crypto.randomUUID()) { this.call = call; this.mint = mint; }
  subscribe = listener => { this.listeners.add(listener); return () => this.listeners.delete(listener); };
  getSnapshot = () => this.snapshot;
  changed() {
    const next = [...this.cleanup.values()].filter(record => record.error).map(record => ({key:record.key,session:record.session,label:record.label,error:record.error,busy:!!record.running,retryable:record.retryable&&!this.retired}));
    for(const open of this.opens.values())if(open.error&&!open.closed)next.push({key:`open:${open.request}`,session:open.session,label:open.label,error:open.error,busy:false,retryable:false});
    if(JSON.stringify(next)===JSON.stringify(this.snapshot))return;
    this.snapshot = next;
    for (const listener of this.listeners) listener();
  }
  capacity(records, session) {
    if (this.retired) throw new Error('The resource connection has retired');
    if (records.size >= 64 || [...records.values()].filter(record => record.session === session).length >= 16)
      throw new Error('Resource capacity is full; close an open resource, finish cleanup, or disconnect and reconnect');
  }
  open(session, command, label = 'Resource') {
    if(this.retired)throw new Error('The resource connection has retired');
    const request = this.mint(), record = {request,session,label,scope:command.pane ? {pane:command.pane,generation:command.generation} : undefined,closed:false,seen:false,identity:undefined,error:undefined,promise:Promise.resolve(/** @type {any} */ (undefined))};
    record.reservation = this.reserveOpening(record);
    this.opens.set(request, record);
    record.promise = Promise.resolve().then(() => this.call('resource_open', JSON.stringify({opening_request:request,command}))).then(text => {
      const reply = JSON.parse(text);
      if (reply.request !== request) throw new Error('Resource receipt belongs to another request');
      if (reply.status === 'unavailable') { this.opens.delete(request); record.reservation.cancel(); return reply; }
      if (reply.status !== 'opened' || !validIdentity(reply)) throw new Error('Invalid resource opening receipt');
      record.identity = reply;
      this.observe(this.entries ?? [], this.surfaces);
      return reply;
    }).catch(error => {
      if(record.identity)return record.identity;
      if (error?.notAdmitted) { this.opens.delete(request); record.reservation.cancel(); }
      else record.error=String(error?.message??error);
      this.changed();
      throw error;
    });
    // The tab consumes the same promise; ownership must also survive unmount.
    record.promise.catch(() => {});
    return record;
  }
  owns(request) { return !!request && this.opens.has(request); }
  suppresses(entry) {
    return [...this.cleanup.values()].some(record => record.committed && record.identity && identity(record.identity) === identity(entry))
      || !!entry.opening_request && this.opens.get(entry.opening_request)?.closed === true;
  }
  reserve(session, key, operation, value, onCommit = () => {}, label = 'Resource') {
    if(this.retired)return {commit:()=>{},cancel:()=>{}};
    const existing = this.cleanup.get(key);
    if (existing) return {commit:() => {},cancel:() => {}};
    this.capacity(this.cleanup, session);
    const record = {key,session,label,operation,identity:value,committed:false,cancelled:false,confirmed:false,retryable:true,seen:value && this.visible.has(identity(value)),running:undefined,error:undefined};
    this.cleanup.set(key, record);
    return {
      commit:() => { if (record.committed || record.cancelled) return; record.committed=true; onCommit(); this.run(record); },
      cancel:() => { if (!record.committed) { record.cancelled=true; this.cleanup.delete(key); } },
    };
  }
  reservePanel(session, value, label = 'Resource') {
    if (!validIdentity(value)) throw new Error('Invalid resource closing identity');
    const opening = [...this.opens.values()].find(open => open.identity && identity(open.identity) === identity(value));
    if (opening) return this.reserveOpen(opening);
    if (this.entries && !this.visible.has(identity(value))) return {commit:()=>{},cancel:()=>{}};
    const captured = {...value};
    return this.reserve(session, `panel:${identity(captured)}`, () => this.closePanel(captured), captured, undefined, label);
  }
  reserveOpen(open) {
    return {commit:open.reservation.commit,cancel:()=>{}};
  }
  reserveOpening(open) {
    let reservation;
    reservation = this.reserve(open.session, `open:${open.request}`, async () => {
      let reply;
      try { reply = open.identity ?? await open.promise; }
      catch (error) {
        if (error?.notAdmitted) return;
        if(open.identity)reply=open.identity;
        else throw Object.assign(error,{unresolvedOpening:true});
      }
      if (reply.status === 'unavailable') return;
      const record = this.cleanup.get(`open:${open.request}`);
      if (record) { record.identity=reply; record.seen=this.visible.has(identity(reply)); }
      await this.closePanel(reply);
    }, open.identity, () => { open.closed=true; }, open.label);
    return reservation;
  }
  attachment(session, pane, generation, request) {
    // Reserve before creating an attachment. Unmount never needs fresh capacity.
    if(this.retired)throw new Error('The resource connection has retired');
    let promise;
    const reservation=this.reserve(session, `attachment:${this.mint()}`, async () => {
      let value;
      try { value=await promise; } catch(error) { if(error?.notAdmitted)return;throw Object.assign(error,{unresolvedOpening:true}); }
      await this.call('terminal', JSON.stringify({pane,generation,request:{type:'detach',attachment:value.id}}));
    }, undefined, undefined, 'Terminal');
    promise=Promise.resolve().then(()=>this.call('terminal', JSON.stringify({pane,generation,request}))).then(text=>JSON.parse(text).value);
    promise.catch(error=>{if(error?.notAdmitted)reservation.cancel();else reservation.commit();});
    return {promise,close:reservation.commit};
  }
  closePanel(value) {
    return this.call('command', JSON.stringify({action:'close_detail',view:value.view,expected_open_revision:value.open_revision}));
  }
  run(record) {
    if (!record.committed || record.running || record.confirmed || this.retired) return;
    record.error=undefined;
    record.running=Promise.resolve().then(record.operation).then(() => {
      record.confirmed=true;
      // Panel suppression lasts until a complete frame confirms absence. A
      // successful close reply may overtake a frame captured before the close.
      if (!record.identity) this.finish(record);
    }, error => {
      record.error=String(error?.message??error);record.unresolvedOpening=!!error?.unresolvedOpening;
      record.retryable=!record.unresolvedOpening&&error?.retryable!==false;
    }).finally(() => {
      record.running=undefined;
      // A matching frame may arrive while the rejected opening is settling.
      if(record.unresolvedOpening&&record.identity){record.unresolvedOpening=false;record.retryable=true;this.run(record);}
      this.changed();
    });
    this.changed();
  }
  retry(key) { const record=this.cleanup.get(key); if(record?.committed&&record.retryable)this.run(record); }
  finish(record) {
    if (this.cleanup.get(record.key)!==record) return;
    this.cleanup.delete(record.key);
    if (record.key.startsWith('open:')) this.opens.delete(record.key.slice(5));
    for (const [request, open] of this.opens) if (open.closed && open.identity && record.identity && identity(open.identity)===identity(record.identity)) this.opens.delete(request);
  }
  observe(entries, surfaces) {
    this.entries=entries; this.surfaces=surfaces;
    this.visible=new Set(entries.map(identity));
    for(const [request, open] of this.opens){
      if(!open.identity){
        const entry=entries.find(entry=>entry.opening_request===request&&validIdentity(entry));
        if(entry){
          open.identity={status:'opened',request,...entry};open.error=undefined;
          const cleanup=this.cleanup.get(`open:${request}`);
          if(cleanup){cleanup.identity=open.identity;cleanup.retryable=true;this.run(cleanup);}
        }
      }
      if(!open.identity)continue;
      if(this.visible.has(identity(open.identity)))open.seen=true;
      else if(!open.closed && (open.seen || open.scope && surfaces && surfaces[open.scope.pane]?.generation !== open.scope.generation)){this.opens.delete(request);open.reservation.cancel();}
    }
    for (const record of this.cleanup.values()) {
      if (!record.committed || !record.identity) continue;
      if (this.visible.has(identity(record.identity))) record.seen=true;
      else if (record.confirmed || record.seen) this.finish(record);
    }
    this.changed();
  }
  retire(error) {
    this.retired=true;
    if (error) { for (const record of this.cleanup.values()) record.error=[record.error,`Connection cleanup failed: ${String(error?.message??error)}`].filter(Boolean).join("; "); }
    else { this.opens.clear(); this.cleanup.clear(); }
    this.changed();
  }
}
