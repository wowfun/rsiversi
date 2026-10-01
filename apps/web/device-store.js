import {emptyDock,validateDock,dockIntent} from './src/dock-state.ts';
import {defaultLayout,validateLayout,parseLayout,workspacePreference} from './presentation-layout.js';

export const budgets = Object.freeze({
  preferences:{record:4096,count:64,total:256*1024},
  orders:{record:128*1024,count:16,total:2*1024*1024},
  layouts:{record:32*1024,count:64,total:2*1024*1024},
});
export const defaultPreferences = Object.freeze({view:'workspace',sessionOrder:'updated',workspaceOrder:'updated'});
const bytes = value => new TextEncoder().encode(JSON.stringify(value)).length;
const plain = value => value && typeof value === 'object' && !Array.isArray(value);
const keys = (value, names) => plain(value) && Object.keys(value).sort().join() === names.split(',').sort().join();
const identity = value => typeof value === 'string' && value.length > 0 && bytes(value) <= 2048 && !/[\x00-\x1f\x7f]/.test(value);
const revision = value => typeof value === 'string' && /^(0|[1-9][0-9]{0,19})$/.test(value) && BigInt(value) <= 18446744073709551615n;
const ids = values => Array.isArray(values) && values.length <= 1024 && values.every(id=>typeof id==='string' && new TextEncoder().encode(id).length<=256 && /^[A-Za-z0-9._:-]+$/.test(id)) && new Set(values).size===values.length;
function defaults(bucket,scope='') {
  if(bucket==='layouts' && scope.startsWith('dock:'))return emptyDock();
  if(bucket==='layouts')return structuredClone(defaultLayout);
  if(bucket==='preferences')return {...defaultPreferences};
  if(bucket==='orders')return {ids:[]};
  throw new Error('Unknown presentation store');
}
function validate(bucket,value) {
  if(bucket==='layouts') {
    if(value?.kind==='session-dock')return validateDock(value);
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
  if(bucket!=='layouts' || value?.kind!=='session-dock')validate(bucket,value);
  if(!plain(intent))throw new Error('Invalid presentation intent');
  let result;
  if(bucket==='layouts' && value?.kind==='session-dock')return dockIntent(value,intent);
  if(bucket==='orders')result=orderIntent(value,intent);
  else if(bucket==='layouts' && keys(intent,'kind,id,expanded') && intent.kind==='workspace') {
    if(typeof intent.expanded!=='boolean' || typeof intent.id!=='string' || !/^[A-Za-z0-9_-]{1,128}$/.test(intent.id))throw new Error('Invalid workspace expansion');
    result=workspacePreference(value,intent.id,intent.expanded);
  } else {
    if(!keys(intent,'kind,patch') || intent.kind!=='patch' || !plain(intent.patch) || Object.keys(intent.patch).some(key=>!Object.hasOwn(value,key) || key==='version' || key==='workspaces'))throw new Error('Invalid presentation patch');
    result={...value,...intent.patch};
    if(bucket==='layouts')result=parseLayout(result);
  }
  return validate(bucket,result);
}

export function boundedRecords(bucket,records,key,value,now) {
  return boundedValidatedRecords(bucket,records,key,validate(bucket,value),now);
}

// Only intent results or the public validating wrapper enter this synchronous path.
function boundedValidatedRecords(bucket,records,key,value,now,unchanged=false) {
  const bound=budgets[bucket];
  if(!bound || records.length>bound.count || !identity(key) || !Number.isSafeInteger(now) || now<0)throw new Error('Invalid presentation storage');
  let total=0;const seen=new Set(),sizes=new Map();
  for(const record of records){
    if(!keys(record,'key,value,revision,used') || !identity(record.key) || !revision(record.revision) || !Number.isSafeInteger(record.used) || record.used<0 || seen.has(record.key))throw new Error('Invalid presentation record');
    seen.add(record.key);
    const size=bytes(record);if(size>bound.record)throw new Error('Presentation record is too large');total+=size;sizes.set(record.key,size);
  }
  if(total>bound.total)throw new Error('Presentation storage is full');
  const previous=records.find(record=>record.key===key);
  if(unchanged)return {records,replacement:previous??{key,value,revision:'0',used:now},changed:false};
  const nextRevision=BigInt(previous?.revision??'0')+1n;
  if(nextRevision>18446744073709551615n)throw new Error('Presentation revision exhausted');
  const replacement={key,value,revision:String(nextRevision),used:now};
  const replacementSize=bytes(replacement);
  if(replacementSize>bound.record)throw new Error('Presentation record is too large');
  const next=records.filter(record=>record.key!==key);
  total=total-(sizes.get(key)??0)+replacementSize;
  if(next.length+1>bound.count || total>bound.total){
    next.sort((a,b)=>a.used-b.used || (a.key<b.key?-1:1));
    while(next.length+1>bound.count || total>bound.total)total-=sizes.get(next.shift().key);
  }
  next.push(replacement);
  return {records:next,replacement,changed:true};
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
  static async open(key) {
    if(!identity(key))throw new Error('Invalid presentation identity');
    return new Promise((resolve,reject)=>{
      const opening=indexedDB.open('rsi.presentation',2);
      opening.onupgradeneeded=event=>{
        const db=opening.result,tx=opening.transaction;
        let legacy;
        if(event.oldVersion===1 && db.objectStoreNames.contains('layouts')){legacy=tx.objectStore('layouts');legacy.name='legacy-layouts-v1';}
        for(const bucket of Object.keys(budgets))if(!db.objectStoreNames.contains(bucket))db.createObjectStore(bucket,{keyPath:'key'});
        if(legacy){
          const store=tx.objectStore('layouts'),read=legacy.getAll(undefined,65);
          read.onsuccess=()=>{
            try{
              if(read.result.length>64)throw new Error('Too many legacy layouts');
              const records=read.result.map(record=>{
                if(!keys(record,'key,layout,used'))throw new Error('Invalid legacy layout');
                return {key:record.key,value:validateLayout(record.layout),used:record.used,revision:'1'};
              });
              if(records.length)boundedRecords('layouts',records,records[0].key,records[0].value,records[0].used);
              for(const record of records)store.put(record);
            }catch{/* Retain the original store for recovery; active layouts start empty. */}
          };
        }
      };
      opening.onerror=()=>reject(opening.error);
      let blocked=false;
      opening.onblocked=()=>{blocked=true;reject(new Error('Presentation storage upgrade is blocked'));};
      opening.onsuccess=()=>{
        if(blocked){opening.result.close();return;}
        const owner=new DeviceStore(opening.result,key);
        opening.result.onversionchange=()=>owner.close();resolve(owner);
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
          result={value:record?validate(bucket,record.value):defaults(bucket,scope),revision:record?.revision??'0'};
        }catch(error){failure=error;tx.abort();}
      };
      tx.oncomplete=()=>resolve(result);
      tx.onabort=tx.onerror=()=>reject(failure??tx.error??new Error('Presentation read failed'));
    });
  }
  async apply(bucket,intent,scope='') {
    return (await this.applyAll([{bucket,intent,scope}]))[0];
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
          const current=index=>{const {bucket,key,scope}=selected[index],record=snapshots[index].find(record=>record.key===key);return record?record.value:defaults(bucket,scope)};
          const preferences=manualPreference?validate('preferences',current(1)):null;
          selected.forEach(({bucket,intent,key,scope},index)=>{
            const records=snapshots[index],value=current(index);
            const observe=manualPreference && (bucket==='preferences' || preferences[manualPreference]!=='manual');
            const applied=observe?validate(bucket,value):applyIntent(bucket,value,intent);
            const next=boundedValidatedRecords(bucket,records,key,applied,Date.now(),applied===value);
            results[index]=next.replacement;
            if(!next.changed)return;
            changed.add(index);
            const store=tx.objectStore(bucket),retained=new Set(next.records.map(record=>record.key));
            for(const record of records)if(!retained.has(record.key))store.delete(record.key);
            store.put(next.replacement);
          });
        }catch(error){failure=error;tx.abort();}
      };
      selected.forEach(({bucket},index)=>{
        const read=tx.objectStore(bucket).getAll(undefined,budgets[bucket].count+1);
        read.onsuccess=()=>{snapshots[index]=read.result;if(--remaining===0)apply()};
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
