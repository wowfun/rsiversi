// Explicit live milestone acceptance. Never imported by the deterministic suite.
import assert from 'node:assert/strict';
import {readFile,writeFile,mkdir,copyFile,chmod,readdir} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium} from 'playwright';
import {startService,boundedRun} from './service.mjs';
import {startMcpFixture} from './mcp-fixture.mjs';
const report=process.env.RSI_WEB_REPORT,model=process.env.RSI_LIVE_MODEL;
assert(process.env.RSI_LIVE_ENV_FILE && report && model && process.env.RSI_WEB_ASSETS,'explicit live inputs required');
assert(/^[a-zA-Z0-9_.-]{1,128}$/.test(model));
const bytes=await readFile(process.env.RSI_LIVE_ENV_FILE);assert(bytes.length<=65536);
const match=bytes.toString('utf8').match(/^\s*(?:export\s+)?DEEPSEEK_API_KEY\s*=\s*(.*?)\s*$/m);assert(match,'authorized key missing');
const key=match[1].trim().replace(/^(["'])(.*)\1$/,'$2');assert(key.length>0);
await mkdir(report,{recursive:true});const binary=join(report,'rsi');await copyFile(process.env.RSI_WEB_BINARY??resolve('target/debug/rsi'),binary);await chmod(binary,0o700);
const mcp=await startMcpFixture({requireCredential:false}),browser=await chromium.launch();
let service,page;const errors=[],started=Date.now();let approvals=0;
const bodyMarker='BODY_ONLY_7e29c6af';
async function complete(pane,input,prompt) {
  await input.fill(prompt);await pane.getByRole('button',{name:'Send ↗',exact:true}).click();
  const deadline=Date.now()+180000;let sawRunning=false;
  while(Date.now()<deadline) {
    const status=await pane.locator('.pane-status').innerText();if(status!=='Completed')sawRunning=true;
    const review=pane.locator('.pending button').filter({hasText:'Review:'});
    if(await review.count() && await review.first().isVisible()){await review.first().click();await page.getByRole('button',{name:'Allow once',exact:true}).click();approvals++;}
    if((sawRunning || await input.inputValue()==='') && ['Completed','Failed'].includes(status))break;
    await page.waitForTimeout(100);
  }
  assert.equal(await pane.locator('.pane-status').innerText(),'Completed',(await pane.locator('.transcript').innerText()).slice(-4096));
}
function facts(session) {
  // --history intentionally returns one 128-Fact page. Read the bounded prior
  // pages through the public interactive CLI before asserting early Tool calls.
  const records=service.run(['--profile','evidence-cli','--resume',session,'--output','jsonl'],{input:':history\n'.repeat(64)+':exit\n'}).stdout.trim().split('\n').map(line=>JSON.parse(line));
  const facts=records.flatMap(record=>record.fact?[record.fact]:[]).sort((a,b)=>a.seq-b.seq);
  assert(facts.length>0 && facts[0].seq===1,'bounded history did not reach the Session baseline');
  assert(new Set(facts.map(fact=>fact.seq)).size===facts.length,'history pages overlap');return facts;
}
function called(records,name) {
  return records.filter(fact=>fact.type==='tool_intent' && fact.name===name).map(intent=>({intent,result:records.find(fact=>fact.type==='tool_result' && fact.effect_id===intent.effect_id && JSON.stringify(fact.identity)===JSON.stringify(intent.identity))}));
}
try {
  service=await startService({binary,assets:process.env.RSI_WEB_ASSETS,report,deepseekKey:key,configure:async({config,workspace})=>{
    boundedRun('git',['init','--quiet',workspace]);
    await writeFile(join(config,'settings.json'),JSON.stringify({'rsi.agent':{default_model:{deployment:'live',model},default_reasoning_effort:'off'},'rsi.retrieval':{web_fetch:true,web_search:true},'rsi.mcp':{servers:[{id:'fixture',enabled:true,tools:['echo'],transport:{kind:'streamable_http',url:mcp.url}}]}}));
    await writeFile(join(config,'host-profiles/fixture/host.profile.toml'),`format=1\n[[steps]]\nkind="plugin"\nid="live"\nplugin="rsi.ai.provider.deepseek"\n[steps.config]\ndeployment="live"\nendpoint="https://api.deepseek.com"\ncredential={owner="rsi.ai.provider.deepseek",slot="default"}\n[steps.config.language_models.${model}]\ncontext_window_tokens=128000\ndefault_output_reserve_tokens=4096\nmax_output_reserve_tokens=16384\n[steps.config.reasoning_efforts.${model}]\nsupported=["off","low","high","max"]\ndefault="off"\n`);
    const cli=join(config,'application-profiles/evidence-cli');await mkdir(cli,{recursive:true});await writeFile(join(cli,'application.profile.toml'),'format=1\n[[steps]]\nkind="plugin"\nid="connection"\nplugin="rsi.application.connection"\nconfig={host_profile="fixture"}\n[[steps]]\nkind="plugin"\nid="cli"\nplugin="rsi.application.cli"\n');
    for(const [name,flags,text] of [['live-check','',`The exact test token is ${bodyMarker}. Use this token in your final response after the requested checks.`],['human-only','disable-model-invocation: true\n','This user-only text must not be returned by skill_read.']]) {
      const directory=join(workspace,'.agents/skills',name);await mkdir(directory,{recursive:true});await writeFile(join(directory,'SKILL.md'),`---\nname: ${name}\ndescription: Isolated live verification skill\n${flags}---\n\n${text}\n`);
    }
    await writeFile(join(workspace,'live-report.txt'),'Recorded present fixture · 中文\n');
  }});
  const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});page=await context.newPage();page.setDefaultTimeout(30000);page.on('pageerror',error=>errors.push(error.message));
  await page.goto(service.origin);await page.locator('#receipt').fill(JSON.stringify(service.register('milestone live browser')));await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});
  await page.locator('.workspace-add summary').click();await page.getByLabel('Server directory').fill(service.workspace);await page.getByRole('button',{name:'Add workspace',exact:true}).click();await page.locator('#workspace-trust').check();await page.locator('#workspaces .nav-item').first().click();await page.getByRole('button',{name:'Trajectory',exact:true}).click();
  const main=page.getByRole('region',{name:'Main conversation',exact:true}),input=main.getByRole('textbox',{name:'Main message',exact:true});
  await input.fill('/live');await main.getByRole('option').filter({hasText:'/live-check'}).waitFor();await main.getByRole('button',{name:'Preview skill',exact:true}).click();await main.locator('.resource-preview-text').filter({hasText:bodyMarker}).waitFor();await page.screenshot({path:join(report,'live-skill-preview.png')});await main.getByRole('button',{name:'Close preview',exact:true}).click();await input.press('Escape');await input.fill('');
  await complete(main,input,'Run this isolated integration check using exactly the named tools. First call skill_read with name live-check. Then call skill_read with name human-only to verify that model invocation is denied; continue after that expected refusal and do not try to read its file. Call mcp__fixture__echo with message LIVE_MCP_ECHO. Call mcp_resource_read for server fixture, first list its resources, then read resource:0 and instructions. Call web_fetch on https://example.com/. Call web_search with query "isolated credential boundary" once; missing Exa credentials is expected, do not work around it. Call present for the existing file live-report.txt with a short description. Do not use any other tools or change files. Finally reply LIVE_MILESTONE_DONE followed by the exact token read from live-check and the fetched page title.');
  const source=(await main.locator('.pane-session').innerText()).split(' · ').at(-1);const first=facts(source);await writeFile(join(report,'source-facts.json'),JSON.stringify(first,null,2));
  for(const name of ['skill_read','mcp__fixture__echo','mcp_resource_read','web_fetch','web_search','present'])assert(called(first,name).length>0,`actual durable ToolIntent missing: ${name}`);
  const skill=called(first,'skill_read').find(call=>call.intent.arguments.name==='live-check');assert(skill?.result && !skill.result.result.is_error && JSON.stringify(skill.result).includes(bodyMarker));
  const manual=called(first,'skill_read').find(call=>call.intent.arguments.name==='human-only');assert(manual,'user-only read was not attempted');assert(!JSON.stringify(manual.result??{}).includes('This user-only text'));
  const fetch=called(first,'web_fetch')[0];assert(fetch.result && !fetch.result.result.is_error);assert.equal(fetch.result.result.value.sources[0].title,'Example Domain');
  const search=called(first,'web_search')[0];assert.equal(search.result.result.value.error,'missing_credential');
  assert(called(first,'present').some(call=>call.result && !call.result.result.is_error));assert(mcp.evidence.calls>=1);assert.equal(service.provider.requests.length,0);
  await main.getByText('Recorded external sources.',{exact:false}).waitFor();await page.screenshot({path:join(report,'live-recorded-sources.png')});await main.locator('article.message').filter({has:page.locator('.message-title').filter({hasText:/^present ·/})}).scrollIntoViewIfNeeded();await main.getByRole('button',{name:'Open current file',exact:true}).click();await main.locator('.inline-card pre').filter({hasText:'Recorded present fixture · 中文'}).scrollIntoViewIfNeeded();await page.screenshot({path:join(report,'live-present-current-file.png')});await main.getByRole('button',{name:'Release snapshot',exact:true}).click();await page.screenshot({path:join(report,'live-tools-and-sources.png')});
  await page.getByRole('button',{name:'+ Compare',exact:true}).click();await page.locator('#workspaces .nav-item').first().click();const compare=page.getByRole('region',{name:'Compare conversation',exact:true}),other=compare.getByRole('textbox',{name:'Compare message',exact:true});
  await compare.getByRole('button',{name:'Reference session',exact:true}).click();const dialog=page.getByRole('dialog',{name:'Reference a conversation',exact:true});await dialog.getByRole('textbox',{name:'Source Session ID',exact:true}).fill(source);await dialog.getByRole('button',{name:'Capture preview',exact:true}).click();await dialog.locator('pre').filter({hasText:bodyMarker}).waitFor();await page.screenshot({path:join(report,'live-reference-capture.png')});await dialog.getByRole('button',{name:'Add to draft',exact:true}).click();
  await complete(compare,other,'Use reference_read exactly once. Copy recorded_session_id, fact_seq, and content_index verbatim from the attached reference\'s final "Read more with reference_read using" line. The source Session and "through Fact" identify captured provenance; the reader locator identifies where the reference was recorded. Set offset 0 and maximum 8192. This is a reader integration test, so call it even if the preview already contains the answer. Use no other tools. Reply LIVE_REFERENCE_DONE and the exact BODY_ONLY token from the frozen conversation.');
  const target=(await compare.locator('.pane-session').innerText()).split(' · ').at(-1),second=facts(target);await writeFile(join(report,'target-facts.json'),JSON.stringify(second,null,2));assert(called(second,'reference_read').some(call=>call.result && !call.result.result.is_error && JSON.stringify(call.result).includes(bodyMarker)));await compare.locator('article.user').first().scrollIntoViewIfNeeded();await compare.locator('.inline-card').filter({hasText:'Source conversation:'}).waitFor({state:'visible'});await page.screenshot({path:join(report,'live-frozen-reference.png')});
  await page.setViewportSize({width:420,height:860});await page.screenshot({path:join(report,'live-reference-narrow.png')});await page.setViewportSize({width:1440,height:980});
  const transcript=await compare.locator('.transcript').innerText();assert(transcript.includes('LIVE_REFERENCE_DONE'));await writeFile(join(report,'target-transcript.txt'),transcript);
  await page.locator('#sign-out').click();await page.locator('#login').waitFor({state:'visible'});assert.deepEqual(errors,[]);
  await writeFile(join(report,'result.json'),JSON.stringify({status:'passed',browser:browser.version(),model,elapsed_ms:Date.now()-started,source,target,approvals,mcp:mcp.evidence,public_fetch:true,exa:'missing-credential boundary only',mock_model_requests:0,clean_sign_out:true},null,2));
} catch(error) {
  if(page){await page.screenshot({path:join(report,'failure.png')}).catch(()=>{});await writeFile(join(report,'failure.txt'),String(error).replaceAll(key,'[REDACTED]'));}
  throw new Error(String(error).replaceAll(key,'[REDACTED]'));
} finally {
  await browser.close();await service?.close();await mcp.close();
  for(const name of await readdir(report))if(/\.(json|log|txt)$/.test(name)){
    const path=join(report,name),text=await readFile(path,'utf8');if(text.includes(key)){await writeFile(path,text.replaceAll(key,'[REDACTED]'));throw new Error('Live evidence contained a credential and was redacted');}
  }
}
