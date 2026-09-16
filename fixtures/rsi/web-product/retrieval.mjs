import assert from 'node:assert/strict';
import {join} from 'node:path';
import {writeFile} from 'node:fs/promises';
export async function verifyRetrieval(page,pane,service,report,browser) {
  const draft=pane.getByRole('textbox',{name:'Main message',exact:true});
  await draft.fill('Retained while configuring web retrieval');
  const before=service.provider.requests.length;
  await page.getByRole('button',{name:'Settings',exact:true}).click();await page.getByRole('button',{name:'Plugins',exact:true}).click();
  const panel=page.getByRole('region',{name:'Web retrieval',exact:true});await panel.waitFor();
  const edit=async(fetch,search)=>{
    await panel.getByRole('button',{name:'Web retrieval settings',exact:true}).click();
    const first=page.getByRole('checkbox',{name:'Settings / web_fetch',exact:true}),second=page.getByRole('checkbox',{name:'Settings / web_search',exact:true});
    await first.waitFor();assert.equal(await first.isChecked(),false);assert.equal(await second.isChecked(),false);
    await first.setChecked(fetch);await second.setChecked(search);
    await page.screenshot({path:join(report,`${browser}-retrieval-settings.png`)});
    const old=await first.elementHandle();await page.getByRole('button',{name:'Save settings',exact:true}).click();await page.waitForFunction(element=>!element.isConnected,old);await old.dispose();
    await page.getByRole('button',{name:'Close details',exact:true}).click();
  };
  await edit(true,true);
  await panel.locator('summary').click();const key=panel.getByLabel('Exa API key',{exact:true});assert(await key.isDisabled());
  await panel.getByRole('button',{name:'Read Exa credential status',exact:true}).click();await key.fill('isolated-exa-fixture-secret');
  assert.equal(await key.getAttribute('type'),'password');
  await panel.getByRole('button',{name:'Save Exa credential',exact:true}).click();await panel.getByRole('status').filter({hasText:'Exa credential saved'}).waitFor();assert.equal(await key.inputValue(),'');
  assert.equal(service.provider.requests.length,before,'configuration cannot submit model requests');
  await panel.getByRole('button',{name:'Read Exa credential status',exact:true}).click();await panel.getByText('configured · editable',{exact:true}).waitFor();
  await panel.screenshot({path:join(report,`${browser}-retrieval.png`)});await page.setViewportSize({width:420,height:860});await panel.evaluate(element=>element.scrollIntoView({block:'center'}));await page.screenshot({path:join(report,`${browser}-retrieval-narrow.png`)});assert(await panel.evaluate(element=>element.scrollWidth<=element.clientWidth+1));await page.setViewportSize({width:1440,height:980});
  assert(!await panel.innerText().then(text=>text.includes('isolated-exa-fixture-secret')||text.includes('store_path')));
  await panel.getByRole('button',{name:'Remove Exa credential',exact:true}).click();await panel.getByRole('status').filter({hasText:'Exa credential removed'}).waitFor();
  await panel.getByRole('button',{name:'Web retrieval settings',exact:true}).click();
  const fetch=page.getByRole('checkbox',{name:'Settings / web_fetch',exact:true}),search=page.getByRole('checkbox',{name:'Settings / web_search',exact:true});
  await fetch.waitFor();assert(await fetch.isChecked());assert(await search.isChecked());await fetch.uncheck();await search.uncheck();const old=await fetch.elementHandle();
  await page.getByRole('button',{name:'Save settings',exact:true}).click();await page.waitForFunction(element=>!element.isConnected,old);await old.dispose();await page.getByRole('button',{name:'Close details',exact:true}).click();
  await page.getByRole('button',{name:'Close settings',exact:true}).click();assert.equal(await draft.inputValue(),'Retained while configuring web retrieval');await draft.fill('');
  await writeFile(join(report,`${browser}-retrieval.json`),JSON.stringify({status:'passed',typed_flags:true,default_disabled:true,credential_saved_and_removed:true,no_implicit_model_request:true,draft_retained:true}));
}
