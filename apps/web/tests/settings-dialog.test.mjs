import test from 'node:test';
import assert from 'node:assert/strict';
import {SettingsDialog} from '../settings-dialog.js';
import {DocumentConnection} from '../document-connection.js';

test('an explicit read fences the dismissed revision even after close delivery failed', async () => {
  const owner = {}; let failures = 0;
  const dialog = new SettingsDialog({close(){},send:async()=>{throw new Error('busy')},failed(){failures++}});
  assert(dialog.update(owner,'1'));
  dialog.dismiss();
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(failures,1);
  assert.equal(dialog.update(owner,'1'),false);
  dialog.begin(owner,'1');
  assert.equal(dialog.update(owner,'1'),false);
  assert(dialog.update(owner,'2'));
});

test('dismissal stays local while the ordinary lane is full and cleanup is pending', async () => {
  const calls = [];
  const transport = {postMessage(message) {
    calls.push(message);
    if (message.method === 'connect') queueMicrotask(() => this.onmessage({data:{kind:'reply',id:message.id,result:'{"endpoint_id":"fixture"}'}}));
  },terminate(){}};
  const owner = new DocumentConnection({transport,mounts:{render(){},close(){}},isCurrent:()=>true,onFrame:()=>({accepted:true}),onFailure(){}});
  await owner.authenticate({},async()=>({}));
  const pending = Array.from({length:8},()=>owner.request('command','{}').catch(()=>{}));
  let closed = 0;
  const dialog = new SettingsDialog({close(){closed++;},send:(owner,revision)=>owner.request('command',JSON.stringify({action:'close_settings',revision})),failed(){}});
  assert(dialog.update(owner,'1'));
  dialog.dismiss();
  assert.equal(closed,1);
  await new Promise(resolve=>setImmediate(resolve));
  assert(calls.some(call=>call.payload === '{"action":"close_settings","revision":"1"}'));
  assert.equal(dialog.update(owner,'1'),false);
  await owner.retire(); await Promise.all(pending);
});

test('dismissed frames and old connection failures cannot affect a new dialog', async () => {
  const a = {}, b = {}; let fail, failures = 0;
  const dialog = new SettingsDialog({close(){},send:()=>new Promise((_,reject)=>{fail=reject;}),failed(){failures++;}});
  assert(dialog.update(a,'1')); dialog.dismiss();
  await Promise.resolve();
  assert.equal(dialog.update(a,'1'),false);
  assert(dialog.update(a,'2'));
  assert(dialog.update(b,'1'));
  fail(new Error('old connection'));
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(failures,0);
  assert(dialog.update(b,'1'));
  for (const revision of ['01','-1','0','18446744073709551616',1]) assert.equal(dialog.update(b,revision),false);
});
