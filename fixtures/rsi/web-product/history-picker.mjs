// Keyless document behavior; controlled History replies, no Service or provider claim.
import assert from 'node:assert/strict';
import {readFile, mkdir, writeFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';

const report=process.env.RSI_REPORT_DIR;
assert(report,'explicit new RSI_REPORT_DIR required');
await mkdir(report,{recursive:false});
const server=await sourceFixture({name:'history-fixture',source:`
import React,{useState} from 'react';
import {createRoot} from 'react-dom/client';
import {HistoryPicker} from './src/session/history-picker.tsx';
import {createHistoryPicker} from './src/runtime/history-picker.js';
import './styles.css';
window.requests=[];
let picker,refresh;
const request=async request=>{
 window.requests.push(request);
 const progress={visible_sources:1,pending_sources:0,discovery_complete:true,continuation:'discover-'+request.scope.kind};
 if(request.operation==='discover')return {kind:'progress',progress,sources:[]};
 if(request.operation==='query')return {kind:'matches',progress,matches:[{label:'Source',scope:{workspace:'workspace-a',conversation:{kind:'native',id:'saved'}},reference_allowed:true,hit:{preview:request.query,original:{record:{kind:'human',sequence:'1'},end:7}}}],next:{query:request.query,scope:request.scope}};
 if(request.operation==='read')return {text:'中text',offset:0,next_offset:7,has_more:false};
 if(request.operation==='freeze')return {reference:{preview:'中',snapshot:{capture_id:'one'}}};
 throw new Error('unexpected History request');
};
function Scene(){const [open,setOpen]=useState(true),[,update]=useState(0);refresh=()=>update(x=>x+1);
 if(!picker){picker=createHistoryPicker({context:{workspace:'workspace-a',session:'target'},workspaces:[{id:'workspace-a',path:'/a'},{id:'workspace-b',path:'/b'}],sources:[],request,release:async()=>{},add:reference=>window.adopted=reference,active:()=>true,changed:()=>refresh(),closed:()=>setOpen(false)});picker.id='history';picker.restoreFocus=()=>document.querySelector('#composer').focus();}
 return <><input id="composer"/>{open&&<HistoryPicker state={picker}/>}</>;
}
createRoot(document.getElementById('root')).render(<Scene/>);
`});
const results=[];
try {for(const [name,engine] of Object.entries({chromium,firefox})) {
 const browser=await engine.launch();
 try {
  const page=await browser.newPage(),errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  await page.goto(server.url);
  const dialog=page.getByRole('dialog',{name:'Search conversation text'}),query=dialog.getByLabel('History text query');
  await query.waitFor();await page.waitForFunction(()=>!document.querySelector('[aria-label="History text query"]').disabled);
  await query.fill('first');await dialog.getByRole('button',{name:'Search text',exact:true}).click();
  await dialog.getByRole('button',{name:'More matches',exact:true}).waitFor();
  await query.fill('second');
  assert.equal(await dialog.getByRole('button',{name:'More matches',exact:true}).count(),0,'edited query retained the old pager');
  await dialog.getByRole('button',{name:'Search text',exact:true}).click();await dialog.getByRole('button',{name:'More matches',exact:true}).waitFor();
  await dialog.getByLabel('History search range').selectOption('workspace');
  assert.equal(await dialog.getByRole('button',{name:'More matches',exact:true}).count(),0,'changed range retained the old pager');
  await dialog.getByLabel('History workspace').selectOption('workspace-b');
  await dialog.getByRole('button',{name:'Continue indexing',exact:true}).click();
  await page.waitForFunction(()=>!document.querySelector('[aria-label="History text query"]').disabled);
  await dialog.getByRole('button',{name:'Search text',exact:true}).click();await dialog.getByRole('button',{name:'More matches',exact:true}).waitFor();
  await dialog.getByRole('button',{name:'More matches',exact:true}).click();
  await page.waitForFunction(()=>!document.querySelector('[aria-label="History text query"]').disabled);
  const requests=await page.evaluate(()=>window.requests);
  const discovery=requests.findLast(request=>request.operation==='discover');
  assert.deepEqual(discovery.scope,{kind:'workspace',workspace:'workspace-b'});assert.equal(discovery.after,null);
  const paged=requests.at(-1);assert.equal(paged.query,'second');assert.deepEqual(paged.scope,discovery.scope);assert.equal(paged.after.query,'second');
  await dialog.getByRole('button',{name:'Open original',exact:true}).click();
  await dialog.getByLabel('Verified original text').evaluate(node=>node.setSelectionRange(0,1));
  await dialog.getByRole('button',{name:'Freeze selected fragment',exact:true}).click();
  await dialog.getByRole('button',{name:'Add selected reference to draft',exact:true}).click();
  await dialog.waitFor({state:'detached'});
  assert.equal(await page.evaluate(()=>window.adopted.preview),'中');
  assert.equal(await page.locator('#composer').evaluate(node=>node===document.activeElement),true);
  assert.equal(await page.evaluate(()=>window.requests.at(-1).end),3);
  assert.deepEqual(errors,[]);
  results.push({engine:name,status:'passed',requests,errors,browser:browser.version()});
 } finally {await browser.close();}
}} finally {await server.close();}
await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));
