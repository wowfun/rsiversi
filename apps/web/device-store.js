import {emptyDock,validateDock,restoreDock,savedDock,dockIntent} from './src/dock-state.ts';
import {defaultLayout,validateLayout,parseLayout,workspacePreference} from './presentation-layout.js';
import {storedNamespace,readNamespace,idBytes,idHex} from './storage-namespace.js';

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
const webDockScope=/^dock:web-([0-9a-f]{32})$/;
function storedScope(scope) {
  const web=webDockScope.exec(scope);
  return web?idBytes(web[1]):scope;
}
function layoutAddress(key) {
  // Shell layouts and other opaque namespaces keep their original string keys.
  if(typeof key!=='string'||key.length>2048)throw new Error('Invalid presentation scope');
  if(!key.startsWith('["['))return key;
  let parts;
  try{parts=JSON.parse(key);}catch{return key;}
  if(!Array.isArray(parts)||parts.length!==2||typeof parts[0]!=='string'||typeof parts[1]!=='string'||!parts[1].startsWith('dock:')||JSON.stringify(parts)!==key)return key;
  const owner=/^\["([0-9a-f]{32})","(local|device:[0-9a-f]{32})"\]$/.exec(parts[0]);
  return owner?[...storedNamespace(owner[1],owner[2]),storedScope(parts[1])]:key;
}
function unpackLayout(record) {
  if(typeof record.key==='string'){
    if(layoutAddress(record.key)!==record.key)throw new Error('Invalid presentation scope');
    return record;
  }
  const key=record.key;
  if(!Array.isArray(key)||key.length!==3)throw new Error('Invalid presentation scope');
  let scope=key[2];
  if(scope instanceof ArrayBuffer&&scope.byteLength===16)scope='dock:web-'+idHex(scope);
  else if(typeof scope!=='string'||!scope.startsWith('dock:')||scope.length>1024||webDockScope.test(scope))throw new Error('Invalid presentation scope');
  const owner=readNamespace(key[0],key[1]);
  if(!owner)throw new Error('Invalid presentation namespace');
  record.key=JSON.stringify([JSON.stringify(owner),scope]);
  return record;
}
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
  if(!plain(intent))throw new Error('Invalid presentation intent');
  if(bucket==='layouts' && value?.kind==='session-dock')return savedDock(dockIntent(restoreDock(value),intent));
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
  static async open(key,{onVersionChange=()=>{}}={}) {
    if(!identity(key))throw new Error('Invalid presentation identity');
    return new Promise((resolve,reject)=>{
      const opening=indexedDB.open('rsi.presentation',7);
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
                if(event.oldVersion===2){
                  let scoped;try{scoped=JSON.parse(key)}catch{/* Unscoped workbench key. */}
                  const dock=Array.isArray(scoped)&&scoped.length===2&&identity(scoped[0])&&typeof scoped[1]==='string'&&scoped[1].startsWith('dock:');
                  if(!dock){store.delete(key);continue;}
                }
                let address;
                if(event.oldVersion===5&&Array.isArray(key)){
                  if(key.length!==3||typeof key[2]!=='string')throw new Error('Invalid presentation scope');
                  const scope=storedScope(key[2]);
                  address=scope===key[2]?key:[key[0],key[1],scope];
                }else address=event.oldVersion>=6?key:layoutAddress(key);
                const oldWidth=event.oldVersion===3&&record.value?.version===4&&Object.hasOwn(record.value,'resourcesWidth');
                if(oldWidth){delete record.value.resourcesWidth;record.value.version=5;}
                const oldNavigation=record.value?.version===5 && record.value.kind!=='session-dock';
                if(oldNavigation){
                  delete record.value.navigation;
                  delete record.value.navigationWidth;
                  record.value.version=6;
                }
                if(address!==key){
                  record.key=address;
                  store.add(record);
                  store.delete(key);
                }else if(oldNavigation)store.put(record);
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
  async read(bucket,scope='',liveDock) {
    if(this.closed || !budgets[bucket])throw new Error('Presentation storage unavailable');
    const key=this.scopedKey(scope);
    return new Promise((resolve,reject)=>{
      const tx=this.database.transaction(bucket,'readonly'),read=tx.objectStore(bucket).get(bucket==='layouts'?layoutAddress(key):key);let result,failure;
      read.onsuccess=()=>{
        try{
          const record=read.result;
          if(record&&bucket==='layouts')unpackLayout(record);
          if(record && (!keys(record,'key,value,revision,used') || record.key!==key || !revision(record.revision) || !Number.isSafeInteger(record.used) || record.used<0 || bytes(record)>budgets[bucket].record))throw new Error('Invalid presentation record');
          const dock=bucket==='layouts'&&scope.startsWith('dock:');
          const value=record?(dock?restoreDock(record.value,liveDock):validate(bucket,record.value)):defaults(bucket,scope);
          result={value,revision:record?.revision??'0'};
        }catch(error){failure=error;tx.abort();}
      };
      tx.oncomplete=()=>resolve(result);
      tx.onabort=tx.onerror=()=>reject(failure??tx.error??new Error('Presentation read failed'));
    });
  }
  async apply(bucket,intent,scope='',beforeCommit,liveDock) {
    return (await this.#applyAll([{bucket,intent,scope}],undefined,beforeCommit,liveDock))[0];
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
  async #applyAll(changes,manualPreference,beforeCommit,liveDock) {
    if(this.closed || !Array.isArray(changes) || changes.length<1 || changes.length>3 || changes.some(change=>!keys(change,'bucket,intent,scope') || !budgets[change.bucket]) || new Set(changes.map(change=>change.bucket)).size!==changes.length)throw new Error('Invalid presentation transaction');
    const selected=changes.map(change=>({...change,key:this.scopedKey(change.scope)}));
    return new Promise((resolve,reject)=>{
      const tx=this.database.transaction(selected.map(change=>change.bucket),'readwrite');
      const results=new Array(selected.length),snapshots=new Array(selected.length),changed=new Set();let failure,remaining=selected.length;
      const apply=()=>{
        try{
          selected.forEach(({bucket},index)=>{if(bucket==='layouts')snapshots[index].forEach(unpackLayout);});
          const current=index=>{
            const {bucket,key,scope}=selected[index],record=snapshots[index].find(row=>row.key===key);
            if(!record)return defaults(bucket,scope);
            return bucket==='layouts'&&scope.startsWith('dock:')?restoreDock(record.value,liveDock):record.value;
          };
          const preferences=manualPreference?validate('preferences',current(1)):null;
          selected.forEach(({bucket,intent,key,scope},index)=>{
            let records=snapshots[index];
            const dock=bucket==='layouts'&&scope.startsWith('dock:');
            const value=current(index);
            const reset=bucket==='layouts'&&keys(intent,'kind')&&intent.kind==='reset';
            if(reset){if(scope!=='')throw new Error('Reset layout only applies to the workbench');records=records.filter(record=>record.key!==key);}
            const observe=manualPreference && (bucket==='preferences' || preferences[manualPreference]!=='manual');
            let applied;
            if(reset)applied=defaults(bucket);
            else if(observe)applied=validate(bucket,value);
            else if(dock)applied=dockIntent(value,intent);
            else applied=applyIntent(bucket,value,intent);
            const stored=dock?savedDock(applied):applied;
            const next=boundedValidatedRecords(bucket,records,key,stored,Date.now(),!reset&&applied===value);
            results[index]=dock?{...next.replacement,value:applied}:next.replacement;
            if(!next.changed)return;
            changed.add(index);
            const store=tx.objectStore(bucket),retained=new Set(next.records.map(record=>record.key));
            for(const record of records)if(!retained.has(record.key))store.delete(bucket==='layouts'?layoutAddress(record.key):record.key);
            store.put(bucket==='layouts'?{...next.replacement,key:layoutAddress(key)}:next.replacement);
          });
          // A synchronous owner can reserve effects for the actual latest
          // committed layout. Throwing aborts every queued write in this tx.
          beforeCommit?.(results.map(result=>result.value));
        }catch(error){failure=error;tx.abort();}
      };
      selected.forEach(({bucket,key},index)=>{
        const store=tx.objectStore(bucket);
        const loaded=records=>{
          snapshots[index]=records;
          if(--remaining===0)apply();
        };
        const read=store.get(bucket==='layouts'?layoutAddress(key):key);
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
          this.notify(bucket,key,true);
        });
        resolve(results.map(result=>({value:result.value,revision:result.revision})));
      };
      tx.onabort=tx.onerror=()=>reject(failure??tx.error??new Error('Presentation intent was not saved'));
    });
  }
  notify(bucket,key,ownCommit=false){for(const listener of this.listeners)if(listener.bucket===bucket && listener.key===key && (!ownCommit||listener.includeOwn))queueMicrotask(listener.callback);}
  subscribe(bucket,scope,callback,{includeOwn=true}={}){const listener={bucket,key:this.scopedKey(scope),callback,includeOwn};this.listeners.add(listener);return()=>this.listeners.delete(listener);}
  close(){if(this.closed)return;this.closed=true;this.listeners.clear();this.channel?.close();this.database.close();}
}
