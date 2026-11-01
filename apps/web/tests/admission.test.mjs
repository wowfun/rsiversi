import {test} from 'node:test';
import assert from 'node:assert/strict';
import {lane,limits} from '../admission.js';
test('both bridge ends classify terminal traffic independently from document commands',()=>{
 assert.equal(lane('terminal',JSON.stringify({request:{type:'read'}})),'terminalRead');
 assert.equal(lane('terminal',JSON.stringify({request:{type:'write'}})),'terminalWrite');
 for(const type of ['create','takeover','detach','unknown'])assert.equal(lane('terminal',JSON.stringify({request:{type}})),'ordinary');
 assert.equal(lane('terminal','malformed'),'ordinary');assert.equal(lane('command','{}'),'ordinary');
 assert.equal(lane('export_cancel'),'ordinary');
 assert.equal(lane('export_input',JSON.stringify({operation:{kind:'cancel',token:'export'}})),'exportControl');
 for(const kind of ['open','next','unknown'])assert.equal(lane('export_input',JSON.stringify({operation:{kind}})),'ordinary');
 assert.equal(lane('export_input','malformed'),'ordinary');
 assert.equal(lane('export_input',JSON.stringify({operation:{kind:'cancel'},padding:'x'.repeat(16*1024)})),'ordinary');
 assert.equal(lane('command',JSON.stringify({action:'close_detail',view:'18446744073709551615',expected_open_revision:'18446744073709551615'})),'exportControl');
 assert.equal(lane('command',JSON.stringify({action:'close_settings',revision:'1'})),'exportControl');
 assert.equal(lane('disconnect'),'lifecycle');assert.deepEqual(limits,{ordinary:8,terminalRead:32,terminalWrite:8,lifecycle:1,exportControl:8});
});
