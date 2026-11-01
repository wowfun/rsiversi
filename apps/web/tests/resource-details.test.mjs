import assert from 'node:assert/strict';
import test from 'node:test';
import {createResourceDetails} from '../src/runtime/resource-details.js';

test('resource detail routing retains UI action authority and exact source acquisition',async()=>{
  const calls=[],slots=[],owner={},interests=[];
  const render=createResourceDetails({connection:owner,rendererSlots:slots,
    boundCall:()=>async(method,payload)=>{calls.push([method,JSON.parse(payload)]);},
    acquireImage:(...args)=>{interests.push(args);return {result:Promise.resolve('image'),release(){}};}});
  let shown,cleared=0;
  const target={rendererKey:'view-1',surface:'pane',show(key,title,model){shown={key,title,model};},clear(){cleared++;}};
  const ui={ticket:'ui-1',binding:{name:'test'},model:{standard_view:{title:'Details'},actions:[{name:'allowed'}]},busy:false,error:null};
  render({ui_detail:ui},target);
  assert.equal(shown.title,'Details');assert.equal(shown.model.data,ui);assert.equal(slots.length,1);
  await slots[0].host.invoke('allowed',{x:1});
  assert.deepEqual(calls[0],['command',{action:'ui_invoke',ticket:'ui-1',name:'allowed',input:{x:1}}]);
  assert.throws(()=>slots[0].host.invoke('unavailable',{}),/no longer available/);
  ui.busy=true;assert.throws(()=>slots[0].host.invoke('allowed',{}),/no longer available/);
  ui.busy=false;ui.model={standard_view:null,actions:[]};
  assert.throws(()=>slots[0].host.invoke('allowed',{}),/no longer available/);
  ui.model=null;assert.throws(()=>slots[0].host.invoke('allowed',{}),/no longer available/);
  const media={width:2,height:3,bytes:4};
  render({has_media:true,source_media:media,source_detail:{ticket:'source-1',source:{seq:'9',field:{kind:'input_image',index:0}}}},target);
  assert.equal(shown.model.kind,'source');assert.equal(shown.model.data.media,media);
  shown.model.data.acquire();assert.equal(interests[0][0],owner);assert.equal(interests[0][1],media);assert.equal(interests[0][2],'source-1');
  render({},target);assert.equal(cleared,1);
});
