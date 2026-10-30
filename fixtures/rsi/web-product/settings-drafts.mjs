import {openSettings} from './controls.mjs';
import {finishFixture} from './cleanup.mjs';
import {paired} from './paired-env.mjs';
import {startService} from './service.mjs';
import {connectWorkbench} from './browser-fixture.mjs';
import {chromium} from 'playwright';
import {mkdir,writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import assert from 'node:assert/strict';
import {workerConformance} from './worker-conformance.mjs';
const report=process.env.RSI_REPORT_DIR;assert(report);await mkdir(report,{recursive:false});
let service,browser,page,failure;const errors=[],checks=[];
const jsonEditor=async page=>{
  const dialog=page.getByRole('dialog',{name:'Settings',exact:true});
  const switcher=dialog.getByRole('button',{name:'Edit as JSON',exact:true});
  const text=dialog.getByRole('textbox',{name:'Settings JSON',exact:true});
  await Promise.race([switcher.waitFor(),text.waitFor()]);
  if(await switcher.isVisible())await switcher.click();
  await text.waitFor();return text;
};
try{
  service=await startService({binary:paired.binary,assets:paired.assets,report});
  browser=await chromium.launch();const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:900}});
  page=await context.newPage();page.on('pageerror',error=>errors.push(error.message));
  await connectWorkbench(page,service,'Settings drafts');
  await openSettings(page);
  assert.equal(await page.getByRole('banner').count(),1);
  let advancedOpened=false;
  const section=async name=>{
    const previous=await page.evaluate(()=>document.querySelector('form[data-settings-ticket]')?.dataset.settingsTicket??null);
    const button=page.getByRole('navigation',{name:'Settings sections'}).getByRole('button',{name,exact:true});
    const changing=await button.getAttribute('aria-pressed')!=='true';
    await button.click();
    const namespace=name==='General'?'rsi.client':name==='Advanced'&&advancedOpened?'rsi.agent':undefined;
    if(namespace&&changing)await page.waitForFunction(({namespace,previous})=>{const form=document.querySelector('form[data-settings-ticket]');return form?.dataset.settingsNamespace===namespace&&form.dataset.settingsTicket!==previous},{namespace,previous});
    if(name==='Advanced')advancedOpened=true;
  };
  await section('General');const initial=await (await jsonEditor(page)).inputValue();
  const appearance='{"exact":9007199254740993,"negative":-0,"decimal":1.0}';
  await (await jsonEditor(page)).fill(appearance);
  await section('Models');await section('General');
  assert.equal(await (await jsonEditor(page)).inputValue(),appearance);
  checks.push('Models round trip preserves exact Appearance draft');
  await section('Advanced');await page.getByRole('button',{name:'rsi.agent',exact:true}).click();
  const advanced='{"exact":9007199254740995,"negative":-0}';await (await jsonEditor(page)).fill(advanced);
  await section('General');assert.equal(await (await jsonEditor(page)).inputValue(),appearance);
  await section('Advanced');assert.equal(await (await jsonEditor(page)).inputValue(),advanced);
  checks.push('Appearance and Advanced retain separate exact drafts');
  await section('General');
  const second=await browser.newPage({ignoreHTTPSErrors:true,viewport:{width:1440,height:900}});second.on('pageerror',error=>errors.push(error.message));
  await connectWorkbench(second,service,'Concurrent settings writer');
  await openSettings(second);
  await second.getByRole('navigation',{name:'Settings sections'}).getByRole('button',{name:'General',exact:true}).click();
  const current=JSON.parse(await (await jsonEditor(second)).inputValue());
  current.appearance={...current.appearance,theme:current.appearance?.theme==='dark'?'light':'dark'};
  await (await jsonEditor(second)).fill(JSON.stringify(current));
  await second.getByRole('button',{name:'Save settings',exact:true}).focus();
  await second.keyboard.press('Enter');
  await second.getByRole('button',{name:'Edit as JSON',exact:true}).waitFor();
  await second.waitForFunction(()=>document.activeElement.matches('button:not(:disabled),input:not(:disabled),textarea:not(:disabled),select:not(:disabled)')&&document.activeElement.closest('[data-settings-ticket]'));
  await section('Advanced');await section('General');
  assert.equal(await (await jsonEditor(page)).inputValue(),appearance);
  await page.getByRole('alert').filter({hasText:'Settings changed while this draft was open'}).waitFor();
  assert(await page.getByRole('button',{name:'Save settings',exact:true}).isDisabled());
  await page.screenshot({path:join(report,'retained-conflict.png')});
  const geometry=await page.evaluate(()=>({editor:document.querySelector('.settings-editor textarea').getBoundingClientRect().width,host:document.querySelector('.registered-settings').getBoundingClientRect().width}));
  assert(geometry.editor>=geometry.host*.95,JSON.stringify(geometry));
  await page.setViewportSize({width:390,height:844});
  await page.screenshot({path:join(report,'retained-conflict-narrow.png')});
  assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1));
  await page.setViewportSize({width:1440,height:900});
  await page.getByRole('button',{name:'Discard edits and reload',exact:true}).focus();
  await page.keyboard.press('Enter');
  await page.getByRole('button',{name:'Edit as JSON',exact:true}).waitFor();
  await page.waitForFunction(()=>document.activeElement.matches('button:not(:disabled),input:not(:disabled),textarea:not(:disabled),select:not(:disabled)')&&document.activeElement.closest('[data-settings-ticket]'));
  assert.deepEqual(JSON.parse(await (await jsonEditor(page)).inputValue()),current);
  checks.push('Concurrent version change retains the draft and refuses a silent rebase');
  await page.screenshot({path:join(report,'reloaded.png')});
  await page.getByRole('button',{name:'Close settings',exact:true}).click();
  await openSettings(page);
  const links=[['Web retrieval settings','rsi.retrieval']];
  if(process.platform!=='win32')links.unshift(['Edit HTTP endpoints','rsi.mcp']);
  for(const [link,namespace] of links){
    await page.getByRole('button',{name:'Plugins',exact:true}).click();
    if(process.platform==='win32')assert.equal(await page.getByRole('button',{name:'Edit HTTP endpoints',exact:true}).count(),0);
    await page.getByRole('button',{name:link,exact:true}).click();
    await page.locator(`form[data-settings-namespace="${namespace}"]`).waitFor();
    assert.equal(await page.locator('form[data-settings-ticket]').count(),1);
  }
  checks.push('Plugins namespace links reach the requested Advanced editor');
  if(process.platform==='win32')checks.push('Windows has no MCP management editor entry (native Unix Profile authority required)');
  await page.screenshot({path:join(report,'plugin-settings.png')});
  const probe=await browser.newPage({ignoreHTTPSErrors:true});await probe.goto(service.origin);
  await writeFile(join(report,'worker.json'),JSON.stringify(await probe.evaluate(workerConformance,{buildFamily:paired.family,receipt:service.register('R7 Worker conformance')}),null,2));
  assert.deepEqual(errors,[]);
  await writeFile(join(report,'result.json'),JSON.stringify({family:paired.family,checks,errors,initialBytes:new TextEncoder().encode(initial).length},null,2));
}catch(error){
  failure=error;
  if(page)await Promise.allSettled([
    page.screenshot({path:join(report,'failure.png')}),
    page.locator('body').innerText().then(text=>writeFile(join(report,'failure.txt'),text)),
  ]);
  throw error;
}finally{await finishFixture(failure,()=>browser?.close(),()=>service?.close());}
