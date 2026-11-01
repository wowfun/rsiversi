import assert from 'node:assert/strict';
import {mkdir,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';

const report=resolve(process.env.RSI_REPORT_DIR);await mkdir(report,{recursive:false});
const source=`
import React from 'react';
import {createRoot} from 'react-dom/client';
import {ComposerModel} from '/src/session/composer-controls.tsx';
import {ExternalSurface} from '/src/session/external.tsx';
import {externalPaneClass} from '/src/runtime/external-controller.js';
import '/styles.css';
const models=Array.from({length:9},(_,i)=>({deployment:'fixture',model:'model-'+i}));
const data={model:models[0],reasoning_effort:null,effort_profile:{supported:['high'],default:'high'}};
const owner={prepareComposer:async()=>true,action:async()=>{},composerView:{pending:false}};
const External=externalPaneClass({command:async()=>{},perform:work=>work().catch(()=>{})});
const external=new External('external');
window.frame={kind:'external',generation:'1',external:{conversation:{kind:'external',id:'peer'},observed:{connected:true,snapshot:{id:'peer',generation:'1',epoch:'1',endpoint:'Fixture agent',cwd:'/workspace',status:'ready'},permissions:[]},capabilities:{submit:true},blocks:Array.from({length:30},(_,i)=>({key:String(i),role:'assistant',text:'Retained observation '+i+'\\nSecond line\\nThird line',truncated:false,source:{conversation:'peer',epoch:'1',sequence:String(i),kind:'update'}})),following:true,busy:false}};
window.updateExternal=(kind)=>{
 const next=JSON.parse(JSON.stringify(window.frame));
 if(kind==='status')next.external.busy=!next.external.busy;
 if(kind==='append')next.external.blocks.push({...next.external.blocks.at(-1),key:String(next.external.blocks.length),text:'New transcript content'});
 if(kind==='replace')next.external.blocks.at(-1).text+='\\nAdditional observed content';
 if(kind==='reading')next.external.following=false;
 window.frame=next;external.staged={data:next,submitKey:'enter'};external.render(next);
};
external.staged={data:window.frame,submitKey:'enter'};external.render(window.frame);
createRoot(document.getElementById('root')).render(<main style={{display:'grid',gridTemplateColumns:'280px 1fr',gap:32,padding:24,height:'100vh'}}><section><h1>Model controls</h1><ComposerModel owner={owner} data={data} models={models}/></section><section style={{height:'100%',minHeight:0}}><ExternalSurface owner={external} selected={true}/></section></main>);
`;
const fixture=await sourceFixture({name:'composer-history',source}),results=[];
try{
 for(const [name,engine] of Object.entries({chromium,firefox})){
  const browser=await engine.launch();
  try{
   const page=await browser.newPage({viewport:{width:1200,height:820}}),errors=[],checks=[];
   page.setDefaultTimeout(5000);page.on('pageerror',error=>errors.push(error.message));
   const check=async(label,work)=>{try{await work();checks.push({label,passed:true})}catch(error){checks.push({label,passed:false,error:String(error)})}};
   await page.goto(fixture.url);
   const trigger=page.getByRole('button',{name:/^Select model/});
   await trigger.click();const menu=page.getByRole('menu');await menu.focus();await page.keyboard.press('ArrowUp');
   await check('Initial ArrowUp selects the last available item',async()=>assert.equal(await page.evaluate(()=>document.activeElement.textContent),'Manage models'));
   await menu.focus();await page.keyboard.press('ArrowDown');
   await check('Initial ArrowDown selects the first available item',async()=>assert.match(await page.evaluate(()=>document.activeElement.textContent),/^Model /));
   await menu.getByRole('menuitem',{name:/^Model /}).click();
   const search=page.getByRole('textbox',{name:'Search models'});await search.fill('model');await search.evaluate(el=>el.setSelectionRange(3,3));await page.keyboard.press('ArrowLeft');
   await check('Search Left Arrow preserves the submenu and native caret',async()=>{assert(await search.isVisible());assert.deepEqual(await search.evaluate(el=>[el===document.activeElement,el.selectionStart,el.selectionEnd]),[true,2,2])});
   if(!await search.isVisible())await menu.getByRole('menuitem',{name:/^Model /}).click();
   await search.fill('model');await search.evaluate(el=>el.setSelectionRange(3,3));await page.keyboard.press('Shift+ArrowLeft');
   await check('Search Shift Left preserves native selection',async()=>{assert(await search.isVisible());assert.deepEqual(await search.evaluate(el=>[el.selectionStart,el.selectionEnd]),[2,3])});
   if(!await search.isVisible())await menu.getByRole('menuitem',{name:/^Model /}).click();
   await page.screenshot({path:join(report,name+'-model-search.png')});
   await menu.getByRole('menuitem',{name:'Model',exact:true}).focus();await page.keyboard.press('ArrowLeft');
   await check('Menu-item Left Arrow returns to the parent menu',async()=>assert(await menu.getByRole('menuitem',{name:'Manage models'}).isVisible()));
   await page.keyboard.press('Escape');
   const history=page.getByRole('region',{name:'Observed history'});
   await history.evaluate(node=>{
    const descriptor=Object.getOwnPropertyDescriptor(Element.prototype,'scrollTop');
    Object.defineProperty(node,'scrollTop',{get(){return descriptor.get.call(this)},set(value){window.scrollWrites++;descriptor.set.call(this,value)}});
    window.scrollWrites=0;
   });
   const update=async(kind)=>{await page.evaluate(kind=>window.updateExternal(kind),kind);await page.evaluate(()=>new Promise(done=>requestAnimationFrame(()=>requestAnimationFrame(done))))};
   const before=await history.evaluate(node=>{node.scrollTop=node.scrollHeight-node.clientHeight-30;window.scrollWrites=0;return node.scrollTop});
   await update('status');
   await check('Cloned busy-only frames preserve near-tail scroll position',async()=>{assert.equal(await history.evaluate(node=>node.scrollTop),before);assert.equal(await page.evaluate(()=>window.scrollWrites),0)});
   await update('append');
   await check('New blocks follow the tail from near the bottom',async()=>assert(await history.evaluate(node=>node.scrollHeight-node.scrollTop-node.clientHeight<=1)));
   await update('replace');
   await check('Changed text in an existing block follows the tail',async()=>assert(await history.evaluate(node=>node.scrollHeight-node.scrollTop-node.clientHeight<=1)));
   await history.evaluate(node=>{node.scrollTop=0;window.scrollWrites=0});await update('append');
   await check('New content preserves a reader away from the tail',async()=>{assert.equal(await history.evaluate(node=>node.scrollTop),0);assert.equal(await page.evaluate(()=>window.scrollWrites),0)});
   await history.evaluate(node=>{node.scrollTop=node.scrollHeight-node.clientHeight-30;window.scrollWrites=0});await update('reading');const reading=await history.evaluate(node=>node.scrollTop);await update('append');
   await check('Historical paging never resumes tail following',async()=>assert.equal(await history.evaluate(node=>node.scrollTop),reading));
   await page.screenshot({path:join(report,name+'-history.png')});
   results.push({browser:name,version:browser.version(),checks,errors});
  }finally{await browser.close()}
 }
}finally{await fixture.close();await writeFile(join(report,'result.json'),JSON.stringify(results,null,2))}
assert(results.every(result=>!result.errors.length&&result.checks.every(check=>check.passed)),JSON.stringify(results));
