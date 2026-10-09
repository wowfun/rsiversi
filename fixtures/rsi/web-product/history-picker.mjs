// Keyless document behavior; controlled History replies, no Service or provider claim.
import assert from 'node:assert/strict';
import {readFile, mkdir, writeFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {chromium} from 'playwright';

const report=process.env.RSI_REPORT_DIR;
assert(report,'explicit new RSI_REPORT_DIR required');
await mkdir(report,{recursive:false});
const script=await readFile(resolve('apps/web/history-picker.js'),'utf8');
const browser=await chromium.launch();
try {
  const page=await browser.newPage(),errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  await page.route('http://history-picker.test/**',route=>route.fulfill({contentType:route.request().url().endsWith('.js')?'text/javascript':'text/html',body:route.request().url().endsWith('.js')?script:'<!doctype html><input id="composer">'}));
  await page.goto('http://history-picker.test/');
  await page.evaluate(async()=>{
    const {openHistoryPicker}=await import('/history-picker.js');
    window.requests=[];
    const pane={editor:{text:'',images:[],references:[]},generation:'1',index:'main',historyContext:{workspace:'workspace-a',session:'target'},input:document.querySelector('#composer')};
    const button=(label,run)=>{const button=document.createElement('button');button.textContent=label;button.onclick=run;return button;};
    const call=async(_method,payload)=>{
      const request=JSON.parse(payload).operation.request;window.requests.push(request);
      const progress={visible_sources:1,pending_sources:0,discovery_complete:true,continuation:`discover-${request.scope.kind}`};
      if(request.operation==='discover')return JSON.stringify({kind:'progress',progress,sources:[]});
      if(request.operation==='query')return JSON.stringify({kind:'matches',progress,matches:[{label:'Source',scope:{workspace:'workspace-a',conversation:{kind:'native',id:'saved'}},reference_allowed:true,hit:{preview:request.query,original:{record:{kind:'human',sequence:'1'}}}}],next:{query:request.query,scope:request.scope}});
      throw new Error('unexpected History request');
    };
    openHistoryPicker(pane,{catalog:{workspaces:[{id:'workspace-a',coordinates:{path:'/a'}},{id:'workspace-b',coordinates:{path:'/b'}}]}},call,button);
  });
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
  assert.deepEqual(errors,[]);
  await writeFile(join(report,'result.json'),JSON.stringify({status:'passed',requests,errors,browser:browser.version()},null,2));
} finally {await browser.close();}
