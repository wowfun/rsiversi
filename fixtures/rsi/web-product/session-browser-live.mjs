// Explicit opt-in: real DeepSeek, confined Chromium, production UI and durable Facts.
import assert from 'node:assert/strict';
import {readFile,appendFile,writeFile} from 'node:fs/promises';
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {once} from 'node:events';
import {join,resolve} from 'node:path';
import {chromium} from 'playwright';
import {liveFixture} from './live-fixture.mjs';
import {connectWorkbench,openWorkspace} from './browser-fixture.mjs';
import {resources,details,clickDetails,closeDetails,openResource} from './controls.mjs';
import {waitUntil} from './service.mjs';
import {liveTurnTerminal,requireProviderUsage,recoverLiveRead} from './evidence.mjs';
import {paired} from './paired-env.mjs';
import {cleanupFinally} from './cleanup.mjs';

assert(process.env.RSI_TEST_BROWSER_NODE&&process.env.RSI_TEST_BROWSER_CHROMIUM&&process.env.RSI_TEST_BROWSER_USER_RUNTIME,'explicit Linux browser runtime required');
const fixture=spawn(process.env.RSI_TEST_BROWSER_NODE,[resolve('crates/rsi/browser/tests/fixtures/session.mjs')],{stdio:['ignore','pipe','inherit']});
const lines=createInterface({input:fixture.stdout});const [port]=await once(lines,'line');
const origin=`http://127.0.0.1:${port}`;
let live,browser,page,factsSession;const errors=[],networkFailures=[],recoveries=[],durable=new Set(),approved=new Set();let approvals=0;
function facts(id){return live.service.run(['--profile','evidence-cli','--resume',id,'--output','jsonl'],{input:':history\n'.repeat(32)+':exit\n'}).stdout.trim().split('\n').map(line=>JSON.parse(line)).flatMap(row=>row.fact?[row.fact]:[]).sort((a,b)=>a.seq-b.seq);}
async function recoverNetwork(id,networkStart){
  if(await page.locator('#connection-state').innerText()!=='Connection failed')return false;
  assert(networkFailures.slice(networkStart).some(failure=>failure.error==='net::ERR_NETWORK_CHANGED'),'unclassified connection loss');
  assert(recoveries.length<2,'network recovery bound exceeded');
  const recovery={session:id,failed_requests:networkFailures.slice(networkStart),notice:await page.locator('#notice').innerText()};
  recoveries.push(recovery);await writeFile(join(live.report,'network-recoveries.json'),JSON.stringify(recoveries,null,2));
  await page.screenshot({path:join(live.report,`network-change-${recoveries.length}.png`)});
  await page.getByRole('button',{name:'Reconnect with this browser',exact:true}).click();
  await page.locator('#workbench').waitFor({state:'visible'});
  await page.getByLabel('Navigation view',{exact:true}).selectOption('flat');
  await page.locator(`[data-session-id="${id}"] [data-testid=conversation-open]`).click();
  await waitUntil(()=>page.getByRole('region',{name:'Main conversation',exact:true}).getAttribute('data-session-id').then(current=>current===id),'reopen exact Session');
  return true;
}
async function complete(pane,prompt){
  const id=await pane.getAttribute('data-session-id');const after=id&&durable.has(id)?(facts(id).at(-1)?.seq??0):0;const networkStart=networkFailures.length;
  await pane.getByRole('textbox',{name:'Main message',exact:true}).fill(prompt);await pane.getByTestId('composer-send').click();
  await waitUntil(async()=>{
    if(await recoverNetwork(id,networkStart))return false;
    if(!await pane.count())return false;
    const review=pane.locator('.pending button').filter({hasText:'Review:'});
    if(await review.count()&&await review.first().isVisible()){
      const control=await review.first().elementHandle();
      const identity=await control.evaluate(button=>[button.dataset.interactionOwner,button.dataset.interactionId]);
      assert(identity.every(value=>typeof value==='string'&&value.length>0),'exact pending interaction identity required');
      const key=JSON.stringify(identity);
      if(!approved.has(key)){
        await control.click();await page.getByRole('button',{name:'Allow once',exact:true}).click();approved.add(key);approvals++;
        await waitUntil(()=>control.evaluate(button=>!button.isConnected),'approved interaction control retirement');
      }
      await control.dispose();
    }
    const state=await recoverLiveRead(async()=>({current:await pane.getAttribute('data-session-id'),status:await pane.locator('.pane-status').innerText()}),()=>recoverNetwork(id,networkStart));
    if(!state)return false;
    const {current,status}=state;
    if(!['Completed','Failed'].includes(status)||!current)return false;
    const terminal=liveTurnTerminal(facts(current),prompt,after);if(!terminal)return false;
    const submissions=facts(current).filter(fact=>fact.seq>after&&fact.type==='input_message_entered'&&fact.source?.type==='human'&&fact.content?.some(part=>part.type==='text'&&part.text===prompt));
    assert.equal(submissions.length,1,'submitted turn was replayed');assert.equal(terminal.outcome.status,'completed',JSON.stringify(terminal));assert.equal(status,'Completed');durable.add(current);return true;
  },'real model turn',240000);
}
async function browserPanel(pane){await openResource(page,'Service extensions');await clickDetails(page,'Session browser');await details(page).getByLabel('Page URL',{exact:true}).waitFor();return details(page);}
try{
  const runtime={node:process.env.RSI_TEST_BROWSER_NODE,chromium_directory:process.env.RSI_TEST_BROWSER_CHROMIUM,package_directory:resolve('crates/rsi/browser/runtime'),systemd_run:'/usr/bin/systemd-run',user_runtime_directory:process.env.RSI_TEST_BROWSER_USER_RUNTIME,artifact_digest:process.env.RSI_TEST_BROWSER_DIGEST};
  assert(/^[a-f0-9]{64}$/.test(runtime.artifact_digest??''),'digest of installed immutable runtime required');
  live=await liveFixture({configure:async({config})=>{
    const profile=join(config,'host-profiles/fixture/host.profile.toml');
    await appendFile(profile,`\n[[steps]]\nkind="patch"\ntarget="session-browser"\nenabled=true\n[[steps]]\nkind="patch"\ntarget="session-browser"\nconfig=${'{runtime={'+Object.entries(runtime).map(([key,value])=>key+'='+JSON.stringify(value)).join(',')+'}}'}\n[[steps]]\nkind="patch"\ntarget="session-browser-ui"\nenabled=true\n`);
  }});
  browser=await chromium.launch();const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});page=await context.newPage();page.setDefaultTimeout(30000);page.on('pageerror',error=>errors.push(error.message));page.on('requestfailed',request=>networkFailures.push({url:request.url(),error:request.failure()?.errorText,at:Date.now()}));
  await connectWorkbench(page,live.service,'Session browser live');await openWorkspace(page,live.service);
  const pane=page.getByRole('region',{name:'Main conversation',exact:true});
  await complete(pane,'HISTORY_NEEDLE_1008 中文🦀材料. This is a saved history seed. Do not use tools. Reply SEED_RECORDED.');const source=await pane.getAttribute('data-session-id');await writeFile(join(live.report,'seed-facts.json'),JSON.stringify(facts(source),null,2));
  await page.locator('#workspaces [data-testid=workspace-open]').first().click();
  await waitUntil(async()=>{const id=await pane.getAttribute('data-session-id');return id&&id!==source;},'new target Session');
  const target=await pane.getAttribute('data-session-id');factsSession=target;assert(target&&target!==source);
  let panel=await browserPanel(pane);await panel.getByLabel('Page URL',{exact:true}).fill(origin);await clickDetails(page,'Approve and open this exact local origin');
  await panel.locator('.ui-text').filter({hasText:'Shared Session browser'}).waitFor();await clickDetails(page,'Observe page');
  const snapshot=JSON.parse(await panel.locator('pre.ui-text').innerText());const named=name=>snapshot.nodes.find(node=>node.name===name).id;
  await panel.getByLabel('Node ID',{exact:true}).fill(named('Name'));await panel.getByLabel('Text to fill',{exact:true}).fill('Ada');await clickDetails(page,'Fill node');
  await panel.getByLabel('Node ID',{exact:true}).fill(named('Apply'));await clickDetails(page,'Click node');await panel.locator('pre.ui-text').filter({hasText:'Hello Ada'}).waitFor();
  await clickDetails(page,'Capture screenshot');await panel.getByAltText('Current Session browser screenshot').waitFor();
  await waitUntil(()=>panel.getByAltText('Current Session browser screenshot').evaluate(img=>img.naturalWidth===1280),'canonical UI image');
  await panel.getByAltText('Current Session browser screenshot').scrollIntoViewIfNeeded();await page.screenshot({path:join(live.report,'browser-wide.png')});
  await page.setViewportSize({width:420,height:860});await page.waitForFunction(()=>document.querySelector('.resource-dock')?.classList.contains('resource-fullscreen'));await resources(page);await panel.waitFor({state:'visible'});await panel.getByAltText('Current Session browser screenshot').scrollIntoViewIfNeeded();assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1));await page.screenshot({path:join(live.report,'browser-narrow.png')});assert(await panel.evaluate(el=>el.scrollWidth<=el.clientWidth+1));await page.setViewportSize({width:1440,height:980});await closeDetails(page);
  await pane.locator('.composer-extras').evaluate(el=>el.open=true);await pane.getByRole('button',{name:'Search history',exact:true}).click();
  const search=page.getByRole('dialog',{name:'Search conversation text',exact:true});await search.getByLabel('History text query').fill('HISTORY_NEEDLE_1008');
  for(let step=0;step<8;step++){await search.getByRole('button',{name:'Search text',exact:true}).click();await waitUntil(()=>search.getByRole('button',{name:'Search text',exact:true}).isEnabled(),'history query');if(await search.getByRole('button',{name:'Open original',exact:true}).count())break;await search.getByRole('button',{name:'Continue indexing',exact:true}).click();await waitUntil(()=>search.getByRole('button',{name:'Search text',exact:true}).isEnabled(),'history indexing');}
  await search.getByRole('button',{name:'Open original',exact:true}).first().click();const original=search.getByLabel('Verified original text');await original.waitFor();
  await original.evaluate(el=>{el.focus();el.setSelectionRange(0,el.value.indexOf('材料')+2);});await search.getByRole('button',{name:'Freeze selected fragment',exact:true}).click();await search.getByRole('button',{name:'Add selected reference to draft',exact:true}).waitFor();await page.screenshot({path:join(live.report,'history-selected.png')});await search.getByRole('button',{name:'Add selected reference to draft',exact:true}).click();await pane.locator('.draft-references').filter({hasText:source}).waitFor();
  await complete(pane,`Use session_browser on the existing shared browser: status, close its current binding, then open policy {mode:"local_dev",origin:"${origin}"} and url "${origin}". Wait for the human approval. Observe, fill Name with Bob using its latest node token, click Apply using the newest observation, then screenshot once. Do not use scripts or other tools, do not retry uncertain actions. Report the page text and finish BROWSER_LIVE_DONE. The provider does not accept image pixels; use structured page text.`);
  assert(approvals>0,'local Tool open did not request human Approval');
  panel=await browserPanel(pane);await panel.locator('pre.ui-text').filter({hasText:'Hello Bob'}).waitFor();await waitUntil(()=>panel.getByAltText('Current Session browser screenshot').evaluate(img=>img.naturalWidth===1280),'model screenshot visible to human');await page.screenshot({path:join(live.report,'shared-model-page.png')});await closeDetails(page);
  await complete(pane,`Use history_search with no conversation restriction to discover saved history and query HISTORY_NEEDLE_1008 in this workspace. Continue finite indexing if pending. Select the match from source conversation ${source}, read the unchanged hit's original, and report the exact Chinese/emoji text. Do not use other tools. Finish HISTORY_LIVE_DONE.`);
  const records=facts(target);const intents=records.filter(f=>f.type==='tool_intent');const results=records.filter(f=>f.type==='tool_result');
  assert(intents.some(f=>f.name==='history_search'&&f.arguments.operation==='query'&&!f.arguments.conversation));assert(intents.some(f=>f.name==='history_search'&&f.arguments.operation==='read'&&f.arguments.conversation.id===source));
  const screenshot=intents.find(f=>f.name==='session_browser'&&f.arguments.operation==='screenshot');assert(screenshot,'model did not capture screenshot');assert(results.some(f=>f.effect_id===screenshot.effect_id&&!f.result.is_error&&f.result.content?.some(c=>c.type==='image')),'durable screenshot Tool image missing');
  const read=intents.find(f=>f.name==='history_search'&&f.arguments.operation==='read'&&f.arguments.conversation.id===source);assert(results.some(f=>f.effect_id===read?.effect_id&&!f.result.is_error&&f.result.value.text?.includes('中文🦀材料')),'model did not read exact Unicode source text');const usage=requireProviderUsage(records);assert.equal(live.service.provider.requests.length,0);assert.deepEqual(errors,[]);assert.equal(await page.locator("#notice").innerText(),"");await writeFile(join(live.report,"network-failures.json"),JSON.stringify(networkFailures,null,2));
  await writeFile(join(live.report,'facts.json'),JSON.stringify(records,null,2));await writeFile(join(live.report,'result.json'),JSON.stringify({status:'passed',network_recoveries:recoveries,family:paired.family,provider_usage:usage,model:live.model,source,target,approvals,real_confined_http_ws:true,shared_human_model_page:true,history_global_human:true,history_workspace_model:true,unicode_reference:true,durable_tool_image:true,provider_accepts_image_pixels:false,mock_requests:0,browser:browser.version()},null,2));
}catch(error){if(factsSession)await Promise.resolve().then(()=>writeFile(join(live.report,'failure-facts.json'),JSON.stringify(facts(factsSession),null,2))).catch(()=>{});if(live)await writeFile(join(live.report,'network-failures.json'),JSON.stringify(networkFailures,null,2)).catch(()=>{});if(page)await writeFile(join(live?.report??process.env.RSI_WEB_REPORT,'browser-panel-failure.json'),JSON.stringify(await details(page).evaluate(el=>({text:el.innerText.slice(0,65536),errors:[...el.querySelectorAll('.source-error')].map(n=>n.textContent),images:[...el.querySelectorAll('img')].map(n=>({alt:n.alt,width:n.naturalWidth,src:n.getAttribute('src')})),buttons:[...el.querySelectorAll('button')].map(n=>({label:n.textContent,disabled:n.disabled}))})).catch(()=>null),null,2)).catch(()=>{});if(page)await page.screenshot({path:join(live?.report??process.env.RSI_WEB_REPORT,'failure.png')}).catch(()=>{});if(live)await writeFile(join(live.report,'failure.txt'),live.redact(error)).catch(()=>{});throw new Error(live?.redact(error)??String(error));}
finally{
  await cleanupFinally(()=>live?.close(),()=>browser?.close(),async()=>{
    if(fixture.exitCode===null){const exited=once(fixture,'exit');fixture.kill('SIGTERM');await exited;}
  });
}
