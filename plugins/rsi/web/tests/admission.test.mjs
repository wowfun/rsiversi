import {test} from 'node:test';
import assert from 'node:assert/strict';
import {lane,limits} from '../admission.js';
test('both bridge ends classify terminal traffic independently from document commands',()=>{
 assert.equal(lane('terminal',JSON.stringify({request:{type:'read'}})),'terminalRead');
 assert.equal(lane('terminal',JSON.stringify({request:{type:'write'}})),'terminalWrite');
 for(const type of ['create','takeover','detach','unknown'])assert.equal(lane('terminal',JSON.stringify({request:{type}})),'ordinary');
 assert.equal(lane('terminal','malformed'),'ordinary');assert.equal(lane('command','{}'),'ordinary');
 assert.equal(lane('disconnect'),'lifecycle');assert.deepEqual(limits,{ordinary:8,terminalRead:32,terminalWrite:8,lifecycle:1});
});
