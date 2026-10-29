import test from 'node:test';
import assert from 'node:assert/strict';
import {FrameTicket} from '../src/runtime/frame-ticket.js';
import {mergePresentationFrame} from '../src/runtime/presentation-frame.js';

const gate = () => { let resolve; const promise = new Promise(done => {resolve = done}); return {promise,resolve} };

test('commit is exact to connection epoch and frame, with independently awaited renderer work', async () => {
  const epoch = {}, ticket = new FrameTicket(epoch, '9', assert.fail);
  const host = {isConnected:true}, renderer = gate(); ticket.requireHosts([host]);
  assert.equal(ticket.commit({}, '9'), false);
  assert.equal(ticket.commit(epoch, '8'), false);
  assert.throws(() => ticket.finish(), /not committed/);
  ticket.commit(epoch, '9'); await ticket.committed;
  let settled = false; const wait = ticket.wait(renderer.promise).then(() => {settled = true});
  await Promise.resolve(); assert.equal(settled,false);
  renderer.resolve(); await wait; ticket.finish();
});

test('retirement interrupts renderer settlement and late commits cannot restore a ticket', async () => {
  const epoch = {}, ticket = new FrameTicket(epoch, '1', assert.fail);
  ticket.commit(epoch,'1'); await ticket.committed;
  const renderer = gate(), wait = ticket.wait(renderer.promise);
  ticket.abort(new Error('replaced'));
  await assert.rejects(wait,/replaced/);
  assert.equal(ticket.commit(epoch,'1'),false);
  renderer.resolve(); assert.throws(() => ticket.finish(),/replaced/);
});

test('a renderer rejection after retirement remains observed by the ticket race',async()=>{
  const epoch={},ticket=new FrameTicket(epoch,'1',assert.fail);
  ticket.commit(epoch,'1');await ticket.committed;
  let reject;
  const operation=new Promise((_,failed)=>{reject=failed});
  const waiting=ticket.wait(operation);
  ticket.abort(new Error('retired'));
  await assert.rejects(waiting,/retired/);
  reject(new Error('late renderer failure'));
  await new Promise(resolve=>setImmediate(resolve));
});

test('host removal before commit or after renderer work cannot finish successfully', async () => {
  for (const afterCommit of [false,true]) {
    const epoch = {}, ticket = new FrameTicket(epoch,'1',assert.fail), host = {isConnected:true};
    ticket.requireHosts([host]);
    if (afterCommit) { ticket.commit(epoch,'1'); await ticket.committed; }
    host.isConnected = false;
    if (!afterCommit) { ticket.commit(epoch,'1'); await assert.rejects(ticket.committed,/detached/); }
    assert.throws(() => ticket.finish(),/detached/);
    ticket.abort(new Error('test cleanup'));
  }
});

test('a presentation deadline fails its owner and fences a late commit', async () => {
  const failed = gate(), epoch = {}, ticket = new FrameTicket(epoch,'1',failed.resolve,5);
  await assert.rejects(ticket.committed,/did not complete/);
  assert.match((await failed.promise).message,/did not complete/);
  assert.equal(ticket.commit(epoch,'1'),false);
});

test('patch merge preserves unchanged identities and requests resync for the wrong base', () => {
  const block = {key:'a',text:'unchanged'}, pane = {generation:'1',transcript:{blocks:[block]}};
  const view = {surfaces:{main:pane}}, frame = {kind:'patch',frame_id:'2',base_frame_id:'1',sections:{notice:'next'},surfaces:[]};
  const next = mergePresentationFrame(view,'1',frame);
  assert.equal(next.surfaces.main,pane); assert.equal(next.surfaces.main.transcript.blocks[0],block);
  assert.equal(mergePresentationFrame(view,'0',frame),undefined);
  assert.throws(() => mergePresentationFrame(view,'1',{...frame,frame_id:'18446744073709551616'}),/Invalid/);
});
