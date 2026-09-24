import assert from 'node:assert/strict';
import {readFile,writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {createHash} from 'node:crypto';

// Real document, Worker, authenticated API, durable Session and streamed download.
export async function verifyExport(page,service,report,name) {
  const input=page.getByRole('textbox',{name:'Main message',exact:true});
  await input.fill('Export Unicode 界 🦀');
  await page.getByRole('button',{name:'Send ↗',exact:true}).click();
  await page.waitForFunction(()=>document.querySelector('.pane-status')?.textContent==='Completed');
  const before=service.provider.requests.length;
  const evidence=[];
  for (const [args,filename] of [["'../export fixture.json' -f json -i h,m,pie,lpr,last-provider-response",'export fixture.json'],['','']]) {
    const promised=page.waitForEvent('download');
    if(args){await input.fill(`/export ${args}`);await page.getByRole('button',{name:'Send ↗',exact:true}).click();}
    else await page.getByRole('button',{name:'Export',exact:true}).click();
    const download=await promised;
    assert.equal(await download.failure(),null);
    const path=await download.path(),bytes=await readFile(path),text=bytes.toString('utf8');
    if(filename)assert.equal(download.suggestedFilename(),filename);
    assert(text.includes('Export Unicode 界 🦀') && text.includes('Reviewed: Export Unicode'),text.slice(0,2048));
    assert(!text.includes('/export '));
    if(args){const artifact=JSON.parse(text);assert.equal(artifact.last_provider_request.availability,'available');assert.equal(artifact.last_provider_request.effect_id,artifact.last_provider_response.effect_id);assert.equal(artifact.last_provider_response.raw,false);assert.deepEqual(Object.keys(artifact).sort(),['header','last_provider_request','last_provider_response','messages','provider_input_evidence']);}
    await download.saveAs(join(report,`${name}-${args?'all.json':'messages.md'}`));
    evidence.push({filename:download.suggestedFilename(),bytes:bytes.length,sha256:createHash('sha256').update(bytes).digest('hex')});
    await page.getByRole('button',{name:'Export',exact:true}).waitFor();
  }
  assert.equal(service.provider.requests.length,before,'export must not submit a model request');
  assert.equal(await page.evaluate(()=>navigator.serviceWorker.controller),null,'download worker must not control the application');
  const registrations=await page.evaluate(async()=> (await navigator.serviceWorker.getRegistrations()).map(r=>new URL(r.scope).pathname));
  assert.deepEqual(registrations,['/downloads/']);
  await writeFile(join(report,`${name}-export.json`),JSON.stringify({status:'passed',downloads:evidence,modelRequests:before},null,2));
  await input.fill('');
  return evidence;
}
