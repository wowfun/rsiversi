// Opt-in real model acceptance; this file is never loaded by default tests.
import assert from 'node:assert/strict';
import {readFile,writeFile,mkdir,copyFile,chmod,readdir} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium} from 'playwright';
import {startService,waitUntil,boundedRun} from './service.mjs';
const report=process.env.RSI_WEB_REPORT,model=process.env.RSI_LIVE_MODEL;
assert(report&&model&&process.env.RSI_LIVE_ENV_FILE&&process.env.RSI_WEB_ASSETS&&process.env.RSI_WEB_BINARY,'explicit live inputs required');
assert(/^[a-zA-Z0-9_.-]{1,128}$/.test(model));
const environment=await readFile(process.env.RSI_LIVE_ENV_FILE);assert(environment.length<=65536);
const match=environment.toString().match(/^\s*(?:export\s+)?DEEPSEEK_API_KEY\s*=\s*(.*?)\s*$/m);assert(match,'authorized key absent');
const key=match[1].trim().replace(/^(["'])(.*)\1$/,'$2');assert(key.length);
await mkdir(report,{recursive:true});const binary=join(report,'rsi');await copyFile(process.env.RSI_WEB_BINARY,binary);await chmod(binary,0o700);
const tokens=['ROLE_SOURCE_V1_f387e5','ROLE_SOURCE_V2_9ab12c'];
const definition=version=>`---\ndescription: Give a concise isolated review\nmodel: { deployment: live, model: ${model} }\nreasoning_effort: off\nallow: []\n---\nYou are the isolated review subagent. Return exactly ${tokens[version-1]} followed by one short sentence reviewing the supplied task. You have no tools.\n`;
const html='<!doctype html><html><head><meta charset="utf-8"><style>body{font-family:sans-serif;padding:32px;background:#edf7f1;color:#0b5149}button{padding:12px;font-size:20px}</style></head><body><h1>Live model artifact</h1><p>Generated through the real coding tools.</p><button onclick="this.textContent=\'Clicked successfully\'">Click to verify</button></body></html>';
const browser=await chromium.launch();let service,page;const errors=[];let approvals=0;
function facts(session) {
 const records=service.run(['--profile','evidence-cli','--resume',session,'--output','jsonl'],{input:':history\n'.repeat(32)+':exit\n'}).stdout.trim().split('\n').map(line=>JSON.parse(line));
 return records.flatMap(record=>record.fact?[record.fact]:[]).sort((a,b)=>a.seq-b.seq);
}
async function complete(pane,input,prompt) {
 await input.fill(prompt);await pane.getByRole('button',{name:'Send ↗',exact:true}).click();let running=false;
 const expires=Date.now()+180000;
 while(Date.now()<expires){
  const status=await pane.locator('.pane-status').innerText();if(!['Completed','Failed'].includes(status))running=true;
  const review=pane.locator('.pending button').filter({hasText:'Review:'});
  if(await review.count()&&await review.first().isVisible()){await review.first().click();await page.getByRole('button',{name:'Allow once',exact:true}).click();approvals++;}
  if((running||await input.inputValue()==='')&&['Completed','Failed'].includes(status)){assert.equal(status,'Completed',(await pane.locator('.transcript').innerText()).slice(-4096));return;}
  await page.waitForTimeout(100);
 }
 throw new Error('Live turn exceeded 180 seconds');
}
try {
 service=await startService({binary,assets:process.env.RSI_WEB_ASSETS,report,deepseekKey:key,configure:async({config,workspace})=>{
  boundedRun('git',['init','--quiet',workspace]);
  await writeFile(join(config,'settings.json'),JSON.stringify({'rsi.agent':{default_model:{deployment:'live',model},default_reasoning_effort:'off'}}));
  await writeFile(join(config,'host-profiles/fixture/host.profile.toml'),`format=1\n[[steps]]\nkind="plugin"\nid="live"\nplugin="rsi.ai.provider.deepseek"\n[steps.config]\ndeployment="live"\nendpoint="https://api.deepseek.com"\ncredential={owner="rsi.ai.provider.deepseek",slot="default"}\n[steps.config.language_models.${model}]\ncontext_window_tokens=128000\ndefault_output_reserve_tokens=4096\nmax_output_reserve_tokens=16384\n[steps.config.reasoning_efforts.${model}]\nsupported=["off","low","high","max"]\ndefault="off"\n`);
  const cli=join(config,'application-profiles/evidence-cli');await mkdir(cli,{recursive:true});await writeFile(join(cli,'application.profile.toml'),'format=1\n[[steps]]\nkind="plugin"\nid="connection"\nplugin="rsi.application.connection"\nconfig={host_profile="fixture"}\n[[steps]]\nkind="plugin"\nid="cli"\nplugin="rsi.application.cli"\n');
  await mkdir(join(workspace,'.agents/agents'),{recursive:true});await writeFile(join(workspace,'.agents/agents/live-reviewer.md'),definition(1));
 }});
 const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});page=await context.newPage();page.setDefaultTimeout(30000);page.on('pageerror',error=>errors.push(error.message));
 await page.goto(service.origin);await page.locator('#receipt').fill(JSON.stringify(service.register('preview live verification')));await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});
 await page.locator('.workspace-add summary').click();await page.getByLabel('Server directory').fill(service.workspace);await page.getByRole('button',{name:'Add workspace',exact:true}).click();await page.locator('#workspaces .nav-item').first().click();await page.getByRole('button',{name:'Trajectory',exact:true}).click();
 const main=page.getByRole('region',{name:'Main conversation',exact:true}),input=main.getByRole('textbox',{name:'Main message',exact:true});
 await input.fill('@live');await main.getByRole('option').filter({hasText:'live-reviewer'}).waitFor();await main.getByRole('button',{name:'Preview agent',exact:true}).click();await main.locator('.resource-preview-text').filter({hasText:tokens[0]}).waitFor();await page.screenshot({path:join(report,'live-agent-definition.png')});await main.getByRole('button',{name:'Close preview',exact:true}).click();await input.press('Escape');
 const children=[];let parent;
 for(let version=1;version<=2;version++){
  if(version===2)await writeFile(join(service.workspace,'.agents/agents/live-reviewer.md'),definition(2));
  await complete(main,input,`@live-reviewer This is isolated acceptance phase ${version}. Use spawn_agent exactly once with role live-reviewer and task_name phase-${version}, message "Review whether a pure function returning 42 is deterministic". Omit fork_turns, model and reasoning_effort so the role defaults and completed history apply. Use wait_agent if needed to observe completion. Do not create other children or edit files. Quote the child's entire reply verbatim, including its role/source identifier, then summarize its result.`);
  parent=(await main.locator('.pane-session').innerText()).split(' · ').at(-1);
  let parentFacts=facts(parent);const intent=parentFacts.find(fact=>fact.type==='tool_intent'&&fact.name==='spawn_agent'&&fact.arguments.task_name===`phase-${version}`);assert(intent,'live spawn ToolIntent absent');
  const result=parentFacts.find(fact=>fact.type==='tool_result'&&fact.effect_id===intent.effect_id);assert(result&&!result.result.is_error,'live spawn failed');const child=result.result.value.session_id;assert(child);
  let childFacts;await waitUntil(()=>{childFacts=facts(child);return childFacts.some(fact=>fact.type==='turn_terminal')},'live child completion',120000);
  await writeFile(join(report,`child-${version}-facts.json`),JSON.stringify(childFacts,null,2));
  const answer=childFacts.filter(fact=>fact.type==='model_event'&&fact.event?.type==='content_delta'&&fact.event.delta?.type==='text').map(fact=>fact.event.delta.value).join('');
  assert(answer.includes(tokens[version-1]),'child did not produce the current definition token');
  assert.equal(childFacts.filter(fact=>fact.type==='tool_intent').length,0,'empty allow child used tools');
  children.push(child);
  await page.screenshot({path:join(report,`live-agent-phase-${version}.png`)});
 }
 await complete(main,input,`Use bash to write exactly the following UTF-8 single-page HTML to live-preview.html in the current workspace, then call present for live-preview.html with description "Interactive live artifact". Do not modify any other file. HTML:\n${html}\nFinally reply LIVE_PREVIEW_READY.`);
 assert.equal((await readFile(join(service.workspace,'live-preview.html'),'utf8')).trimEnd(),html);
 await main.getByRole('button',{name:'Open current file',exact:true}).click();const frame=page.frameLocator('iframe.file-html');await frame.getByRole('heading',{name:'Live model artifact'}).waitFor();await frame.getByRole('button',{name:'Click to verify'}).click();await frame.getByRole('button',{name:'Clicked successfully'}).waitFor();await page.screenshot({path:join(report,'live-html-interactive.png')});
 const parentFacts=facts(parent);await writeFile(join(report,'parent-facts.json'),JSON.stringify(parentFacts,null,2));
 const parentAnswer=parentFacts.filter(fact=>fact.type==='model_event'&&fact.event?.type==='content_delta'&&fact.event.delta?.type==='text').map(fact=>fact.event.delta.value).join('');
 for(const [index,token] of tokens.entries()){
  const completion=parentFacts.find(fact=>fact.type==='input_message_entered'&&fact.source?.type==='completion'&&fact.source.child_session_id===children[index]&&fact.source.outcome.status==='completed');
  assert(completion?.content.some(part=>part.type==='text'&&part.text.includes(token)),'durable child completion did not carry the current reply');
  assert(parentAnswer.includes(token),'main agent did not quote the child reply');
 }
 assert.equal(service.provider.requests.length,0);assert.deepEqual(errors,[]);assert.equal(await page.locator('#notice').innerText(),'');
 await page.locator('#sign-out').click();await page.locator('#login').waitFor({state:'visible'});
 await writeFile(join(report,'result.json'),JSON.stringify({status:'passed',model,parent,children,definition_versions:2,next_spawn_refresh:true,empty_allow:true,real_tool_created_html:true,interactive_preview:true,mock_model_requests:0,approvals,browser:browser.version()},null,2));
}catch(error){if(page){await page.screenshot({path:join(report,'failure.png')}).catch(()=>{});await writeFile(join(report,'failure.html'),(await page.content().catch(()=>'' )).replaceAll(key,'[REDACTED]'));}throw new Error(String(error).replaceAll(key,'[REDACTED]'));}
finally{
 await browser.close();await service?.close();
 for(const entry of await readdir(report,{withFileTypes:true})){if(entry.isFile()&&/\.(json|txt|html|log)$/.test(entry.name)){const path=join(report,entry.name),text=await readFile(path,'utf8');if(text.includes(key)){await writeFile(path,text.replaceAll(key,'[REDACTED]'));throw new Error('Credential appeared in evidence and was redacted');}}}
}
