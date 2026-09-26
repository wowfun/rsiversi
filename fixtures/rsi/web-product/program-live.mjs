import {openBrowserPage, connectWorkbench, openWorkspace} from './browser-fixture.mjs';
import {requireProviderUsage} from './evidence.mjs';
import {cleanupAll} from './cleanup.mjs';
// Explicit live comparison: same files and model, measured provider usage only.
import assert from 'node:assert/strict';
import {writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {chromium} from 'playwright';
import {liveFixture, configureProgram} from './live-fixture.mjs';
import {assertNoNotices} from './task-checks.mjs';
const fixture=await liveFixture({configure:async({config,workspace})=>{
  await configureProgram({config});
  for(let index=0;index<4;index++)await writeFile(join(workspace,`sample-${index}.json`),JSON.stringify({n:index+10,private_payload:`INTERNAL_FILE_${index}_`.repeat(512)}));
}});
const {service,report,model}=fixture,errors=[],measurements=[];
let page,browser;
try {
  ({browser, page} = await openBrowserPage(chromium, errors));
  await connectWorkbench(page, service, 'live program tool calls');
  await openWorkspace(page, service);
  const pane=page.getByRole('region',{name:'Main conversation',exact:true}),input=pane.getByRole('textbox',{name:'Main message',exact:true});
  for(const mode of ['ordinary','program']) {
    if(mode==='program') {
      const previous=await pane.locator('.pane-session').innerText();
      await page.locator('#workspaces .nav-item').first().click();
      await page.waitForFunction(previous=>document.querySelector('[aria-label="Main conversation"] .pane-session')?.textContent!==previous,previous);
    }
    const script="let total=0; for(let i=0;i<4;i++){const r=await tools.call('file_read',{path:`sample-${i}.json`,maximum:32768}); if(r.is_error)throw new Error(JSON.stringify(r.value));total+=JSON.parse(r.value.text).n;} return {total};";
    const instruction=mode==='ordinary'
      ? 'Call file_read directly exactly once for each of sample-0.json, sample-1.json, sample-2.json, sample-3.json (maximum 32768). Do not call run_code, bash or any other tool. Sum their n fields from the returned files.'
      : `Call run_code exactly once with this script, preserving it exactly: ${script} Do not call any other tool directly.`;
    await input.fill(`Isolated integration test. ${instruction} Do not modify files. Finish with LIVE_TOTAL_46 only if the actual tool results sum to 46. Do not repeat the private_payload contents.`);
    const started=Date.now();await pane.getByRole('button',{name:'Send ↗',exact:true}).click();await input.waitFor();await page.waitForFunction(()=>document.querySelector('[aria-label="Main message"]')?.value==='');
    const deadline=Date.now()+180000;
    while(Date.now()<deadline) {
      const status=await pane.locator('.pane-status').innerText();
      if(status==='Failed'||(status==='Completed'&&(await pane.locator('.transcript').innerText()).includes('LIVE_TOTAL_46')))break;
      await page.waitForTimeout(100);
    }
    const transcript=await pane.locator('.transcript').innerText();await writeFile(join(report,`${mode}-transcript.txt`),transcript);assert.equal(await pane.locator('.pane-status').innerText(),'Completed',transcript.slice(-4096));
    const elapsed_ms=Date.now()-started,session=(await pane.locator('.pane-session').innerText()).split(' · ').at(-1);
    const records=service.run(['--profile','evidence-cli','--resume',session,'--output','jsonl'],{input:':history\n'.repeat(128)+':exit\n'}).stdout.trim().split('\n').map(line=>JSON.parse(line));
    const facts=records.flatMap(record=>record.fact?[record.fact]:[]).sort((a,b)=>a.seq-b.seq);await writeFile(join(report,`${mode}-facts.json`),JSON.stringify(facts,null,2));
    const intents=facts.filter(f=>f.type==='tool_intent'),reads=intents.filter(f=>f.name==='file_read');assert.equal(reads.length,4);
    const text=facts.filter(f=>f.type==='model_event').map(f=>f.event?.type==='content_delta'&&f.event.delta?.type==='text'?f.event.delta.value:'').join('');assert.match(text,/LIVE_TOTAL_46/);
    if(mode==='program') {
      const coordinators=intents.filter(f=>f.name==='run_code');assert.equal(coordinators.length,1);assert.equal(coordinators[0].arguments.script,script);
      assert(reads.every((read,index)=>read.origin?.kind==='program'&&read.origin.parent_effect_id===coordinators[0].effect_id&&read.origin.ordinal===index+1));
      const result=facts.find(f=>f.type==='tool_result'&&f.effect_id===coordinators[0].effect_id);assert.equal(result?.result.value.value.total,46);
      assert.match(transcript,/Program ·/);
    } else assert(reads.every(read=>read.origin?.kind==='model'));
    const usage=requireProviderUsage(facts);
    measurements.push({mode,session,elapsed_ms,model_requests:facts.filter(f=>f.type==='model_intent').length,tool_calls:intents.length,usage});
    await page.getByRole('region',{name:'Needs attention',exact:true}).getByText('Running',{exact:true}).waitFor({state:'hidden'});await page.screenshot({path:join(report,`${mode}-wide.png`)});
    if(mode==='program') {await pane.locator('.message-title').filter({hasText:'Program ·'}).first().scrollIntoViewIfNeeded();await page.screenshot({path:join(report,'program-card-wide.png')});}
  }
  await page.setViewportSize({width:420,height:900});await pane.locator('.message-title').filter({hasText:'Program ·'}).first().scrollIntoViewIfNeeded();await page.screenshot({path:join(report,'program-narrow.png')});
  assert.equal(service.provider.requests.length,0);await assertNoNotices(page);assert.deepEqual(errors,[]);
  await writeFile(join(report,'result.json'),JSON.stringify({ok:true,browser:browser.version(),model,reasoning_effort:'off',mock_model_requests:0,measurements,scope:'One isolated paired sample; not a general latency or token improvement claim.'},null,2));
}catch(error){if(page)await page.screenshot({path:join(report,'failure.png')}).catch(()=>{});await writeFile(join(report,'failure.txt'),fixture.redact(error));throw new Error(fixture.redact(error));}
finally {await cleanupAll(() => browser?.close(), () => fixture.close());}
