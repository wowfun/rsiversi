import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {mkdir,readFile,appendFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {startService} from '../web-product/service.mjs';
const require=createRequire(new URL('../web-product/package.json',import.meta.url));
const {chromium,firefox}=require('playwright');
const binary=resolve(process.env.RSI_BINARY??'target/debug/rsi');
const probe=resolve('target/debug/examples/profile-leaf-probe');
const assets=process.env.RSI_WEB_ASSETS;assert(assets);
const output=resolve(process.argv[2]);await mkdir(output,{recursive:true});
const results=[];
for(const [name,engine] of [['chromium',chromium],['firefox',firefox]]) {
  const report=join(output,name);await mkdir(report,{recursive:true});let source;
  const service=await startService({binary,assets,report,async configure({config}){
    const directory=join(config,'host-profiles/editable');await mkdir(directory);
    source=join(directory,'host.profile.toml');await writeFile(source,'format = 1\nsteps = []\n');
  }});
  const local=request=>{const result=JSON.parse(service.probe(probe,request).stdout);assert('Ok' in result,JSON.stringify(result));return result.Ok;};
  const target=local({operation:'catalog',query:{profile:'editable',after:null}}).leaves.find(leaf=>leaf.target.leaf==='rsi-inspector-api')?.target;
  assert(target,'fixture leaf must be on the first source page');
  const registration=service.register(`${name} Profile review`);
  const scope={principal:{kind:'device',id:registration.id},target,operation:'disable'};
  const grant=granted=>local({operation:'set_grant',request:{expected:local({operation:'grants'}).revision,scope,granted}});
  const browser=await engine.launch({headless:true});let page;const errors=[];
  try {
    const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});
    page=await context.newPage();page.setDefaultTimeout(30_000);page.on('pageerror',error=>errors.push(error.message));
    const login=async()=>{await page.locator('#receipt').fill(JSON.stringify(registration));await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});};
    await page.goto(service.origin);await login();
    const open=async()=>{await page.getByRole('button',{name:'Settings',exact:true}).click();await page.getByRole('button',{name:'Plugins',exact:true}).click();await page.getByRole('button',{name:'Browse Host Profiles',exact:true}).click();};
    await open();const panel=page.getByRole('region',{name:'Host Profile management',exact:true});
    await panel.getByRole('button',{name:'Open Profile editable',exact:true}).click();await panel.getByLabel('Plugin leaf',{exact:true}).selectOption(target.leaf);
    assert(await panel.getByRole('button',{name:'Preview disable',exact:true}).isDisabled());
    assert.equal(await panel.getByText('Grant an exact change',{exact:true}).count(),0,'Device must not show Local grant editor');
    grant(true);await panel.getByRole('button',{name:'Refresh source choices',exact:true}).click();
    const preview=()=>panel.getByRole('button',{name:'Preview disable',exact:true}).click();
    await preview();const prepared=panel.getByRole('article',{name:`Prepared ${target.leaf}`,exact:true});await prepared.waitFor();
    assert.equal(await readFile(source,'utf8'),'format = 1\nsteps = []\n');
    await page.screenshot({path:join(report,'review.png')});
    await appendFile(source,'# concurrent source edit\n');
    await prepared.getByRole('button',{name:'Save reviewed change',exact:true}).click();
    const receipt=panel.getByRole('article',{name:'Profile source receipt',exact:true});await receipt.getByText('conflict',{exact:true}).waitFor();
    assert.equal(await readFile(source,'utf8'),'format = 1\nsteps = []\n# concurrent source edit\n');
    await panel.getByRole('button',{name:'Refresh source choices',exact:true}).click();await preview();await prepared.waitFor();
    const ticket=await prepared.locator('dd code').last().innerText();
    await prepared.getByRole('button',{name:'Save reviewed change',exact:true}).click();await receipt.getByText('saved',{exact:true}).waitFor();await receipt.getByText('not selected',{exact:true}).waitFor();
    const saved=await readFile(source,'utf8');assert.match(saved,/kind = "patch"/);await page.screenshot({path:join(report,'saved.png')});
    await receipt.getByRole('button',{name:'Query original receipt',exact:true}).click();await receipt.getByText('saved',{exact:true}).waitFor();assert.equal(await readFile(source,'utf8'),saved);
    await page.getByRole('button',{name:'Close settings',exact:true}).click();await page.locator('#sign-out').click();await page.locator('#login').waitFor({state:'visible'});await login();await open();
    await panel.getByText(/Recover a source receipt/).click();await panel.getByLabel('Original receipt ticket',{exact:true}).selectOption(ticket);await panel.getByRole('button',{name:'Read selected receipt',exact:true}).click();await receipt.getByText('saved',{exact:true}).waitFor();
    await page.setViewportSize({width:430,height:900});await page.screenshot({path:join(report,'narrow.png')});assert(await panel.evaluate(element=>element.scrollWidth<=element.clientWidth+1));
    await page.setViewportSize({width:1440,height:980});await panel.getByRole('button',{name:'Open Profile editable',exact:true}).click();await panel.getByLabel('Plugin leaf',{exact:true}).selectOption(target.leaf);
    grant(false);await panel.getByRole('button',{name:'Refresh source choices',exact:true}).click();assert(await panel.getByRole('button',{name:'Preview disable',exact:true}).isDisabled());
    await receipt.getByRole('button',{name:'Query original receipt',exact:true}).click();await receipt.getByText('saved',{exact:true}).waitFor();assert.equal(await readFile(source,'utf8'),saved);
    assert.equal(service.provider.requests.length,0);assert.deepEqual(errors,[]);
    results.push({engine:name,version:browser.version(),stale_source_rejected:true,source_saved_once:true,original_receipt_after_reconnect:true,revocation:true,model_requests:0,ok:true});
  } catch(error) {if(page)await page.screenshot({path:join(report,'failure.png')}).catch(()=>{});results.push({engine:name,ok:false,error:String(error),page_errors:errors});throw error;}
  finally {await browser.close();await service.close();await writeFile(join(output,'result.json'),JSON.stringify(results,null,2)+'\n');}
}
console.log(JSON.stringify(results));
