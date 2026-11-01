import assert from 'node:assert/strict';
import {mkdir,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';

const report=resolve(process.env.RSI_REPORT_DIR);await mkdir(report,{recursive:true});
const source=`
import React,{useEffect,useRef,useSyncExternalStore} from 'react';
import {createRoot} from 'react-dom/client';
import {mount,unmount} from '/src/runtime/session-elements.js';
import {externalPaneClass} from '/src/runtime/external-controller.js';
import '/styles.css';
window.transportCalls=0;window.refusal='not_admitted';
const External=externalPaneClass({command:async()=>{window.transportCalls++;if(window.refusal)throw Object.assign(Error('Temporary refusal'),{notAdmitted:window.refusal==='not_admitted'})},perform:work=>work().catch(()=>{})});
const external=new External('external');
external.render({generation:'1',external:{observed:{connected:true,snapshot:{id:'external',status:'ready'}},capabilities:{submit:true},busy:false}});
function Probe(){
 const region=useRef(null),editor=useRef(null),input=useRef(null),form=useRef(null);
 useSyncExternalStore(external.subscribe,external.getSnapshot);
 useEffect(()=>{
  const controls=Object.fromEntries(['history','live','commands','bottom','composer','modelReceipt','imageInput','attach','addReference','exportButton','searchHistory','addFile','cancel','steer','send','protectedExport'].map(key=>[key,document.createElement('button')]));
  const pane={index:'main',scheduleVisible(){},setReadingSummary(value){window.readingSummary=value},closeCompletions(){},updateCompletions(){}};
  const context={select(){},TranscriptVisibility:class{dispose(){}}};
  mount(pane,context,{...controls,node:region.current,transcript:region.current,input:document.createElement('textarea')});
  window.pane=pane;
  external.mount({node:form.current,input:input.current});
  return()=>{unmount(pane,context);external.dispose()};
 },[]);
 return <main style={{padding:32,maxWidth:900,margin:'auto'}}><h1>Conversation input checks</h1>
 <section role="region" aria-label="Transcript" tabIndex={0} ref={region} style={{height:260,overflow:'auto',border:'1px solid var(--line)',padding:16}}>
 <label>Inline notes<textarea ref={editor} aria-label="Inline notes" defaultValue={'first line\\nsecond line'} style={{display:'block',width:'100%',height:90}}/></label>
 <div tabIndex={0} role="button" aria-label="Handled End" onKeyDown={event=>event.preventDefault()}>Inline control owns End</div>
 <div style={{height:800}}>Retained transcript content</div><p>Transcript tail</p></section>
 <section ref={form} style={{marginTop:24}}><h2>External conversation</h2><label>Prompt<textarea ref={input} aria-label="External prompt" disabled={external.composerView.inputDisabled} style={{display:'block',width:'100%',height:90}}/></label>
 <button disabled={external.composerView.sendDisabled} onClick={()=>external.submit().catch(()=>{})}>Send</button><p role="status">{external.composerView.notice}</p></section></main>;
}
createRoot(document.getElementById('root')).render(<Probe/>);
`;
const fixture=await sourceFixture({name:'controller-keyboard',source}),results=[];
try{
 for(const [name,engine] of Object.entries({chromium,firefox})){
  const browser=await engine.launch();
  try{
   const page=await browser.newPage({viewport:{width:1000,height:800}}),errors=[];page.on('pageerror',error=>errors.push(error.message));
   await page.goto(fixture.url);await page.waitForFunction(()=>!!window.pane);
   const region=page.getByRole('region',{name:'Transcript'}),editor=page.getByRole('textbox',{name:'Inline notes'});
   await editor.focus();await editor.evaluate(el=>el.setSelectionRange(0,0));
   await page.evaluate(()=>{window.pane.transcript.scrollTop=0;window.readingSummary=true});
   await page.keyboard.press('End');
   assert.equal(await editor.evaluate(el=>el.selectionStart),10);
   assert.equal(await region.evaluate(el=>el.scrollTop),0);assert.equal(await page.evaluate(()=>window.readingSummary),true);
   await editor.evaluate(el=>el.setSelectionRange(0,0));await page.keyboard.press('Shift+End');
   assert.deepEqual(await editor.evaluate(el=>[el.selectionStart,el.selectionEnd]),[0,10]);
   await page.getByRole('button',{name:'Handled End'}).focus();await page.keyboard.press('End');
   assert.equal(await region.evaluate(el=>el.scrollTop),0);
   await region.focus();await page.keyboard.press('End');
   assert.equal(await page.evaluate(()=>window.readingSummary),false);
   assert(await region.evaluate(el=>el.scrollTop+el.clientHeight>=el.scrollHeight-1));
   const prompt=page.getByRole('textbox',{name:'External prompt'}),send=page.getByRole('button',{name:'Send',exact:true});
   await prompt.fill('Retain this prompt');await send.click();await page.waitForFunction(()=>window.transportCalls===1);
   assert.equal(await prompt.inputValue(),'Retain this prompt');assert.equal(await send.isEnabled(),true);
   await page.screenshot({path:join(report,name+'-retry.png')});
   await page.evaluate(()=>window.refusal=null);await send.click();await page.waitForFunction(()=>window.transportCalls===2);
   assert.equal(await prompt.inputValue(),'');
   await page.evaluate(()=>window.refusal='unknown');await prompt.fill('Unknown outcome');await send.click();
   await page.getByRole('status').filter({hasText:'outcome is unknown'}).waitFor();assert.equal(await send.isEnabled(),false);
   await page.screenshot({path:join(report,name+'-unknown.png')});assert.deepEqual(errors,[]);
   results.push({browser:name,version:browser.version(),nativeEditing:true,regionEnd:true,explicitRetry:true,unknownFence:true,errors});
  }finally{await browser.close()}
 }
}finally{await fixture.close();await writeFile(join(report,'result.json'),JSON.stringify(results,null,2))}
