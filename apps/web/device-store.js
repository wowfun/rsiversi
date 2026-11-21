import {defaultLayout,validateLayout,parseLayout,workspacePreference} from './presentation-layout.js';

export const budgets = Object.freeze({
  preferences:{record:4096,count:64},
  orders:{record:128*1024,count:16},
  layouts:{record:32*1024,count:64},
});
export const defaultPreferences = Object.freeze({view:'workspace',sessionOrder:'updated',workspaceOrder:'updated'});
const bytes = value => new TextEncoder().encode(JSON.stringify(value)).length;
const plain = value => value && typeof value === 'object' && !Array.isArray(value);
const keys = (value, names) => plain(value) && Object.keys(value).sort().join() === names.split(',').sort().join();
const identity = value => typeof value === 'string' && value.length > 0 && bytes(value) <= 2048 && !/[\x00-\x1f\x7f]/.test(value);
const revision = value => typeof value === 'string' && /^(0|[1-9][0-9]{0,19})$/.test(value) && BigInt(value) <= 18446744073709551615n;
const ids = values => Array.isArray(values) && values.length <= 1024 && values.every(id=>typeof id==='string' && new TextEncoder().encode(id).length<=256 && /^[A-Za-z0-9._:-]+$/.test(id)) && new Set(values).size===values.length;
function defaults(bucket) {
  if(bucket==='layouts')return structuredClone(defaultLayout);
  if(bucket==='preferences')return {...defaultPreferences};
  if(bucket==='orders')return {ids:[]};
  throw new Error('Unknown presentation store');
}
function validate(bucket,value) {
  if(bucket==='layouts') {
    if(!plain(value) || JSON.stringify(validateLayout(value))!==JSON.stringify(value))throw new Error('Invalid saved layout');
  } else if(bucket==='preferences') {
    if(!keys(value,'view,sessionOrder,workspaceOrder') || !['workspace','workspace_tree','flat'].includes(value.view) || !['updated','manual'].includes(value.sessionOrder) || !['updated','manual'].includes(value.workspaceOrder))throw new Error('Invalid navigation preferences');
  } else if(bucket==='orders') {
    if(!keys(value,'ids') || !ids(value.ids))throw new Error('Invalid saved order');
  } else throw new Error('Unknown presentation store');
  return value;
}
function membership(members) {
  if(!Array.isArray(members) || !ids(members.map(member=>member?.id)) || members.some(member=>!keys(member,'id,partition') || !identity(member.partition)))throw new Error('Invalid complete order membership');
  return new Map(members.map(member=>[member.id,member.partition]));
}
export function reconcileOrder(saved,members) {
  membership(members);
  if(!ids(saved))throw new Error('Invalid saved order');
  const seen=new Set(saved);
  const result=[...saved,...members.filter(member=>!seen.has(member.id)).map(member=>member.id)];
  if(!ids(result))throw new Error('Saved order is full; select Updated to reset it');
  return result;
}
function orderIntent(value,intent) {
  if(keys(intent,'kind') && intent.kind==='updated')return {ids:[]};
  const members=membership(intent.members), ordered=reconcileOrder(value.ids,intent.members);
  if(keys(intent,'kind,members') && intent.kind==='reconcile')return {ids:ordered};
  if(!keys(intent,'kind,members,id,position,relative') || intent.kind!=='move' || !members.has(intent.id) || !['before','after','first','last','previous','next'].includes(intent.position))throw new Error('Invalid order intent');
  const partition=members.get(intent.id), siblings=ordered.filter(id=>members.get(id)===partition);
  const current=siblings.indexOf(intent.id);
  let relative=intent.relative, side=intent.position;
  if(side==='first'){relative=siblings[0];side='before';}
  if(side==='last'){relative=siblings.at(-1);side='after';}
  if(side==='previous'){relative=siblings[Math.max(0,current-1)];side='before';}
  if(side==='next'){relative=siblings[Math.min(siblings.length-1,current+1)];side='after';}
  if(!members.has(relative) || members.get(relative)!==partition)throw new Error('Cannot move across execution or pin partitions');
  if(relative===intent.id)return {ids:ordered};
  ordered.splice(ordered.indexOf(intent.id),1);
  ordered.splice(ordered.indexOf(relative)+(side==='after'?1:0),0,intent.id);
  return {ids:ordered};
}
export function applyIntent(bucket,value,intent) {
  if(!plain(intent))throw new Error('Invalid presentation intent');
  validate(bucket,value);
  let result;
  if(bucket==='orders')result=orderIntent(value,intent);
  else if(bucket==='layouts' && (intent.kind==='workspace' || intent.kind==='reveal_workspace')) {
    const reveal=intent.kind==='reveal_workspace';
    if(!keys(intent,reveal?'kind,id':'kind,id,expanded') || (!reveal && typeof intent.expanded!=='boolean') || typeof intent.id!=='string' || !/^[A-Za-z0-9_-]{1,128}$/.test(intent.id))throw new Error('Invalid workspace expansion');
    if(reveal && value.workspaces.some(item=>item.id===intent.id))return value;
    result=workspacePreference(value,intent.id,reveal || intent.expanded);
  } else {
    if(!keys(intent,'kind,patch') || intent.kind!=='patch' || !plain(intent.patch))throw new Error('Invalid presentation patch');
    const fields=Object.keys(intent.patch);
    if(fields.some(key=>!Object.hasOwn(value,key) || key==='version' || key==='workspaces'))throw new Error('Invalid presentation patch');
    if(fields.every(key=>value[key]===intent.patch[key]))return value;
    result={...value,...intent.patch};
    if(bucket==='layouts')result=parseLayout(result);
  }
  return validate(bucket,result);
}

// The intent owner has validated value. This boundary validates stored envelopes and sizes.
export function planPresentationWrite(bucket,records,key,value,now,unchanged=false) {
  const bound=budgets[bucket];
  if(!bound || records.length>bound.count || !identity(key) || !Number.isSafeInteger(now) || now<0)throw new Error('Invalid presentation storage');
  const seen=new Set();
  for(const record of records){
    if(!keys(record,'key,value,revision,used') || !identity(record.key) || !revision(record.revision) || !Number.isSafeInteger(record.used) || record.used<0 || seen.has(record.key))throw new Error('Invalid presentation record');
    seen.add(record.key);
    if(bytes(record)>bound.record)throw new Error('Presentation record is too large');
  }
  const previous=records.find(record=>record.key===key);
  if(unchanged)return {replacement:previous??{key,value,revision:'0',used:now},changed:false};
  const nextRevision=BigInt(previous?.revision??'0')+1n;
  if(nextRevision>18446744073709551615n)throw new Error('Presentation revision exhausted');
  const replacement={key,value,revision:String(nextRevision),used:now};
  if(bytes(replacement)>bound.record)throw new Error('Presentation record is too large');
  let evicted;
  if(!previous && records.length===bound.count){
    const oldest=records.reduce((oldest,record)=>{
      if(record.used!==oldest.used)return record.used<oldest.used?record:oldest;
      return record.key<oldest.key?record:oldest;
    });
    evicted=oldest.key;
  }
  return {replacement,evicted,changed:true};
}

export class DeviceStore {
  constructor(database,key) {
    this.database=database;this.key=key;this.listeners=new Set();this.closed=false;
    this.channel=typeof BroadcastChannel==='function'?new BroadcastChannel('rsi.presentation.v2'):undefined;
    this.channel?.addEventListener('message',event=>{
      const data=event.data;
      if(keys(data,'bucket,key,revision') && budgets[data.bucket] && identity(data.key) && revision(data.revision))this.notify(data.bucket,data.key);
    });
  }
  static async open(key,{onVersionChange=()=>{}}={}) {
    if(!identity(key))throw new Error('Invalid presentation identity');
    return new Promise((resolve,reject)=>{
      const opening=indexedDB.open('rsi.presentation',8);
      opening.onupgradeneeded=event=>{
        const db=opening.result,tx=opening.transaction;
        if(event.oldVersion<2 && db.objectStoreNames.contains('layouts'))db.deleteObjectStore('layouts');
        if(db.objectStoreNames.contains('legacy-layouts-v1'))db.deleteObjectStore('legacy-layouts-v1');
        for(const bucket of Object.keys(budgets))if(!db.objectStoreNames.contains(bucket))db.createObjectStore(bucket,{keyPath:'key'});
        if(event.oldVersion>=2){
          const store=tx.objectStore('layouts'),read=store.getAll(undefined,budgets.layouts.count+1);
          read.onsuccess=()=>{
            try{
              if(read.result.length>budgets.layouts.count)throw new Error('Too many saved layouts');
              for(const record of read.result){
                const key=record.key;
                if(event.oldVersion===2 || Array.isArray(key) || record.value?.kind==='session-dock'){
                  store.delete(key);
                  continue;
                }
                const oldWidth=event.oldVersion===3&&record.value?.version===4&&Object.hasOwn(record.value,'resourcesWidth');
                if(oldWidth){delete record.value.resourcesWidth;record.value.version=5;}
                const oldNavigation=record.value?.version===5;
                if(oldNavigation){
                  delete record.value.navigation;
                  delete record.value.navigationWidth;
                  record.value.version=6;
                }
                if(oldNavigation||oldWidth)store.put(record);
              }
            }catch{tx.abort();}
          };
        }
      };
      opening.onerror=()=>reject(opening.error);
      let blocked=false;
      opening.onblocked=()=>{blocked=true;reject(new Error('Presentation storage upgrade is blocked'));};
      opening.onsuccess=()=>{
        if(blocked){opening.result.close();return;}
        const owner=new DeviceStore(opening.result,key);
        opening.result.onversionchange=()=>{owner.close();onVersionChange();};resolve(owner);
      };
    });
  }
  scopedKey(scope) {if(typeof scope!=='string' || bytes(scope)>1024)throw new Error('Invalid presentation scope');const key=scope?JSON.stringify([this.key,scope]):this.key;if(!identity(key))throw new Error('Presentation scope key is too large');return key;}
  async read(bucket,scope='') {
    if(this.closed || !budgets[bucket])throw new Error('Presentation storage unavailable');
    const key=this.scopedKey(scope);
    return new Promise((resolve,reject)=>{
      const tx=this.database.transaction(bucket,'readonly'),read=tx.objectStore(bucket).get(key);let result,failure;
      read.onsuccess=()=>{
        try{
          const record=read.result;
          if(record && (!keys(record,'key,value,revision,used') || record.key!==key || !revision(record.revision) || !Number.isSafeInteger(record.used) || record.used<0 || bytes(record)>budgets[bucket].record))throw new Error('Invalid presentation record');
          const value=record?validate(bucket,record.value):defaults(bucket);
          result={value,revision:record?.revision??'0'};
        }catch(error){failure=error;tx.abort();}
      };
      tx.oncomplete=()=>resolve(result);
      tx.onabort=tx.onerror=()=>reject(failure??tx.error??new Error('Presentation read failed'));
    });
  }
  async apply(bucket,intent,scope='') {
    return (await this.#applyAll([{bucket,intent,scope}]))[0];
  }
  async applyAll(changes) {
    return this.#applyAll(changes);
  }
  async reconcileNavigation(scope,members) {
    const preference=scope==='sessions:all'?'sessionOrder':scope==='workspaces'?'workspaceOrder':null;
    if(!preference)throw new Error('Invalid navigation scope');
    membership(members);
    return this.#applyAll([
      {bucket:'orders',scope,intent:{kind:'reconcile',members}},
      {bucket:'preferences',scope:'',intent:{kind:'patch',patch:{}}},
    ],preference);
  }
  async #applyAll(changes,manualPreference) {
    if(this.closed || !Array.isArray(changes) || changes.length<1 || changes.length>3 || changes.some(change=>!keys(change,'bucket,intent,scope') || !budgets[change.bucket]) || new Set(changes.map(change=>change.bucket)).size!==changes.length)throw new Error('Invalid presentation transaction');
    const selected=changes.map(change=>({...change,key:this.scopedKey(change.scope)}));
    return new Promise((resolve,reject)=>{
      const tx=this.database.transaction(selected.map(change=>change.bucket),'readwrite');
      const results=new Array(selected.length),snapshots=new Array(selected.length),changed=new Set();let failure,remaining=selected.length;
      const apply=()=>{
        try{
          const current=index=>{
            const {bucket,key}=selected[index];
            const record=snapshots[index].find(row=>row.key===key);
            return record?record.value:defaults(bucket);
          };
          const preferences=manualPreference?validate('preferences',current(1)):null;
          selected.forEach(({bucket,intent,key,scope},index)=>{
            let records=snapshots[index];
            const value=current(index);
            const reset=bucket==='layouts'&&keys(intent,'kind')&&intent.kind==='reset';
            if(reset){if(scope!=='')throw new Error('Reset layout only applies to the workbench');records=records.filter(record=>record.key!==key);}
            const observe=manualPreference && (bucket==='preferences' || preferences[manualPreference]!=='manual');
            let applied;
            if(reset)applied=defaults(bucket);
            else if(observe)applied=validate(bucket,value);
            else applied=applyIntent(bucket,value,intent);
            const next=planPresentationWrite(bucket,records,key,applied,Date.now(),!reset&&applied===value);
            results[index]=next.replacement;
            if(!next.changed)return;
            changed.add(index);
            const store=tx.objectStore(bucket);
            if(next.evicted!==undefined)store.delete(next.evicted);
            store.put(next.replacement);
          });
        }catch(error){failure=error;tx.abort();}
      };
      selected.forEach(({bucket,key},index)=>{
        const store=tx.objectStore(bucket);
        const loaded=records=>{
          snapshots[index]=records;
          if(--remaining===0)apply();
        };
        const read=store.get(key);
        read.onsuccess=()=>{
          if(read.result!==undefined){loaded([read.result]);return;}
          // Only a new identity can require LRU eviction.
          const admission=store.getAll(undefined,budgets[bucket].count+1);
          admission.onsuccess=()=>loaded(admission.result);
        };
      });
      tx.oncomplete=()=>{
        selected.forEach(({bucket,key},index)=>{
          if(!changed.has(index))return;
          try{this.channel?.postMessage({bucket,key,revision:results[index].revision});}catch{/* Commit remains authoritative if notification delivery fails. */}
          this.notify(bucket,key);
        });
        resolve(results.map(result=>({value:result.value,revision:result.revision})));
      };
      tx.onabort=tx.onerror=()=>reject(failure??tx.error??new Error('Presentation intent was not saved'));
    });
  }
  notify(bucket,key){for(const listener of this.listeners)if(listener.bucket===bucket && listener.key===key)queueMicrotask(listener.callback);}
  subscribe(bucket,scope,callback){const listener={bucket,key:this.scopedKey(scope),callback};this.listeners.add(listener);return()=>this.listeners.delete(listener);}
  close(){if(this.closed)return;this.closed=true;this.listeners.clear();this.channel?.close();this.database.close();}
}
