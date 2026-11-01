import assert from 'node:assert/strict';
import test from 'node:test';
import {createResourceProjector} from '../src/runtime/resource-projector.js';
import {createResourceDetails} from '../src/runtime/resource-details.js';

test('unchanged resource entries retain models while renderer slots are collected each frame',()=>{
  let published=0,previous,observed=0,calls=0;
  const details={connection:{},boundCall:()=>{calls++;return async()=>{};},rendererSlots:[]};
  const hosts=new Map(),context={connection:{resources:{observe(){observed++;}}},resourceHosts:hosts,
    element:()=>({remove(){}}),$:()=>({append(){}}),renderDetail:createResourceDetails(details),
    publishResources(){const model=hosts.get('1')?.model;if(model!==previous){previous=model;published++;}}};
  const render=createResourceProjector(context);
  const entry={view:'1',ui_detail:{ticket:'1',binding:{generation:'1'},model:{standard_view:{title:'Card'},actions:[]},busy:false,error:null}};
  const frame={panels:[entry],surfaces:{},has_media:false};
  for(let i=0;i<100;i++){details.rendererSlots=[];render(frame);assert.equal(details.rendererSlots.length,1);}
  assert.equal(published,1);assert.equal(calls,1);assert.equal(observed,1);
  frame.panels=[{...entry}];render(frame);assert.equal(published,2,'entry replacement rebuilds the model');
  frame.has_media=true;render(frame);assert.equal(published,3,'media capability replacement rebuilds the model');
  details.connection={};render(frame);assert.equal(published,4,'new connection cannot retain old command authority');
  frame.panels=[{view:'1',source_detail:{ticket:'2',text:'x'.repeat(65536)}}];render(frame);
  const source=hosts.get('1').model;
  for(let i=0;i<100;i++)render(frame);
  assert.equal(hosts.get('1').model,source,'unchanged source page retains its exact model');
});

test('an empty resource frame can render before a connection exists',()=>{
  let published=0;
  const render=createResourceProjector({resourceHosts:new Map(),publishResources(){published++;}});
  render({panels:[],surfaces:{},has_media:false});
  assert.equal(published,1);
});

test('resource projection retains hosts and refreshes owner observation on scope changes',()=>{
  let observed=0,removed=0,rendered=0;
  const hosts=new Map();
  const context={connection:{resources:{observe(){observed++;}}},resourceHosts:hosts,
    element:()=>({remove(){removed++;}}),$:()=>({append(){}}),
    renderDetail(){rendered++;},publishResources(){}};
  const render=createResourceProjector(context);
  const frame={panels:[{view:'1',open_revision:'2'}],surfaces:{main:{generation:'1'}},has_media:false};
  render(frame);const host=hosts.get('1');render(frame);
  assert.equal(hosts.get('1'),host);assert.equal(observed,1);assert.equal(rendered,2);
  render({...frame,surfaces:{main:{generation:'2'}}});assert.equal(observed,2);
  render({...frame,panels:[]});assert.equal(removed,1);assert.equal(hosts.size,0);
});
