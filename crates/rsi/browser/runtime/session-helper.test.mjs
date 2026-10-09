import assert from 'node:assert/strict';
import test from 'node:test';
import {sessionHelper} from './session-helper.mjs';

test('page text, node semantics and truncated hrefs serialize as Unicode scalars',async t=>{
  const state={role:'button\ud800',name:'Name\udfff',type:'submit',href:'x'.repeat(511)+'🦀',disabled:false,read_only:false};
  const previousDocument=globalThis.document,previousFilter=globalThis.NodeFilter;
  t.after(()=>{globalThis.document=previousDocument;globalThis.NodeFilter=previousFilter;});
  globalThis.NodeFilter={SHOW_TEXT:4};
  globalThis.document={body:{},createTreeWalker:()=>{let consumed=false;return {nextNode:()=>{if(consumed)return null;consumed=true;return {textContent:'text\ud800\udfff\ud800',parentElement:{tagName:'P'}};}};}};
  const wire=value=>{const pending=[value];while(pending.length){const item=pending.pop();if(typeof item==='string')assert(item.isWellFormed(),'lone surrogate reached the CDP boundary');else if(item&&typeof item==='object')pending.push(...Object.values(item));}return JSON.parse(JSON.stringify(value));};
  const element={tagName:'BUTTON',type:'submit',getAttribute:name=>({role:state.role,'aria-label':state.name,href:state.href}[name]??null)};
  const handle={isVisible:async()=>true,evaluate:async callback=>wire(callback(element)),dispose:async()=>{}};
  const page={setViewportSize:async()=>{},on:()=>{},url:()=> 'https://public.example/',evaluate:async callback=>wire(callback()),locator:()=>({count:async()=>1,nth:()=>({elementHandle:async()=>handle})})};
  const context={pages:()=>[page],on:()=>{},route:async()=>{}};
  const helper=await sessionHelper({contexts:()=>[context]},{mode:'public_web'});
  const result=await helper({operation:'observe'});
  assert.equal(result.status,'completed');
  assert.equal(result.snapshot.text,'text\ud800\udfff�\n');
  assert.equal(result.snapshot.nodes[0].name,'Name�');
  assert.equal(result.snapshot.nodes[0].role,'button�');
  assert.equal(result.snapshot.nodes[0].signature,undefined);
  assert(!/\\u[dD][89a-fA-F][0-9a-fA-F]{2}/.test(JSON.stringify(result)));
  state.name='Name\ud800';
  const refused=await helper({operation:'click',document_version:result.snapshot.document_version,observation_id:result.snapshot.observation_id,node:'1'});
  assert.equal(refused.reason,'node_changed_or_not_actionable','distinct invalid UTF-16 values must not collapse action freshness');
});

test('only the initial blank document allows navigation and screenshots recheck the settled URL',async()=>{
  let url='about:blank',navigation;
  const page={setViewportSize:async()=>{},on:(name,listener)=>{if(name==='framenavigated')navigation=listener;},mainFrame:()=>page,url:()=>url,
    goto:async target=>{url=target;navigation(page);},evaluate:async()=>'',locator:()=>({count:async()=>0}),
    screenshot:async()=>{url='about:blank';navigation(page);return Buffer.from('image');}};
  const context={pages:()=>[page],on:()=>{},route:async()=>{}};
  const helper=await sessionHelper({contexts:()=>[context]},{mode:'public_web'});
  await assert.rejects(helper({operation:'observe'}),/policy/);
  const opened=await helper({operation:'navigate',url:'https://public.example/'});
  assert.equal(opened.status,'completed');
  assert.equal(opened.url,'https://public.example/');
  await assert.rejects(helper({operation:'screenshot'}),/policy/);
  await assert.rejects(helper({operation:'navigate',url:'https://public.example/'}),/policy/);
});

test('observation truncates by encoded UTF-8 structure bytes and retains at most 256 nodes',async()=>{
  for(const name of ['界'.repeat(256),'"\\\n'.repeat(80)]){
    const state={role:'button',name,type:'submit',href:'x'.repeat(8192),disabled:false,read_only:false};
    const handle={isVisible:async()=>true,evaluate:async()=>state,dispose:async()=>{}};
    const page={setViewportSize:async()=>{},on:()=>{},url:()=> 'https://public.example/',evaluate:async()=> '原文'.repeat(4000),locator:()=>({count:async()=>300,nth:()=>({elementHandle:async()=>handle})})};
    const context={pages:()=>[page],on:()=>{},route:async()=>{}};
    const helper=await sessionHelper({contexts:()=>[context]},{mode:'public_web'});
    const result=await helper({operation:'observe'});
    assert.equal(result.status,'completed');assert.equal(result.snapshot.truncated,true);
    assert(result.snapshot.nodes.length>0&&result.snapshot.nodes.length<=256);
    assert(Buffer.byteLength(JSON.stringify({text:result.snapshot.text,nodes:result.snapshot.nodes}))<=60*1024);
    assert.equal(result.snapshot.nodes[0].name,name);
  }
});

test('the complete observation envelope stays within 64 KiB with a long legal URL',async()=>{
  const url='https://public.example/?q='+'x'.repeat(8150);
  const state={role:'button',name:'界'.repeat(256),type:'submit',href:'',disabled:false,read_only:false};
  let disposed=0;
  const handle={isVisible:async()=>true,evaluate:async()=>state,dispose:async()=>{disposed++;}};
  const page={setViewportSize:async()=>{},on:()=>{},url:()=>url,evaluate:async()=> '原文'.repeat(1000),locator:()=>({count:async()=>300,nth:()=>({elementHandle:async()=>handle})})};
  const context={pages:()=>[page],on:()=>{},route:async()=>{}};
  const helper=await sessionHelper({contexts:()=>[context]},{mode:'public_web'});
  const result=await helper({operation:'observe'});
  assert.equal(result.status,'completed');assert.equal(result.snapshot.url,url);
  assert(Buffer.byteLength(url)<=8192);
  assert(Buffer.byteLength(JSON.stringify(result.snapshot))<=64*1024);
  assert.equal(result.snapshot.truncated,true);assert(disposed>0);
  const refused=await helper({operation:'click',document_version:result.snapshot.document_version,observation_id:result.snapshot.observation_id,node:String(result.snapshot.nodes.length+1)});
  assert.equal(refused.status,'not_started');
});
