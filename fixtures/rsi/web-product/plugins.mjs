import assert from 'node:assert/strict';
import {join} from 'node:path';
export async function verifyPlugins(page,pane,service,receipt,report,browser) {
  const input=pane.getByRole('textbox',{name:'Main message',exact:true});
  await input.fill('Retained while inspecting plugins');
  await page.getByRole('button',{name:'Settings',exact:true}).click();
  await page.getByRole('button',{name:'Plugins',exact:true}).click();
  const panel=page.getByRole('region',{name:'Plugins',exact:true});
  await panel.locator('.plugin-rows .plugin-row').first().waitFor();
  const revisions=await panel.locator('.plugin-revisions').innerText();assert.match(revisions,/Desired \d+ · Observed \d+/);
  assert(!await panel.innerText().then(text=>text.includes(service.workspace)));
  assert.match(await panel.innerText(),/active|Not observed/);
  const first=await panel.locator('.plugin-row h3').first().innerText();
  if(await panel.getByRole('button',{name:'Next plugin page',exact:true}).isEnabled()) {
    await panel.getByRole('button',{name:'Next plugin page',exact:true}).click();
    await page.waitForFunction(first=>document.querySelector('.plugin-row h3')?.textContent!==first,first);
    await panel.getByRole('button',{name:'Previous plugin page',exact:true}).click();await panel.locator('.plugin-row h3').filter({hasText:first}).first().waitFor();
  }
  await page.screenshot({path:join(report,`${browser}-plugins.png`)});
  await page.setViewportSize({width:420,height:860});await page.screenshot({path:join(report,`${browser}-plugins-narrow.png`)});
  assert(await panel.evaluate(element=>element.scrollWidth<=element.clientWidth+1));
  await page.setViewportSize({width:1440,height:980});
  const grants=()=>JSON.parse(service.run(['--profile','devices','configuration','list']).stdout);
  service.run(['--profile','devices','configuration','revoke',receipt.id,grants().revision]);
  await panel.getByRole('button',{name:'Refresh plugin status',exact:true}).click();
  await panel.getByRole('alert').filter({hasText:'Plugin status unavailable'}).waitFor();assert.equal(await panel.locator('.plugin-rows .plugin-row').count(),0);
  await page.screenshot({path:join(report,`${browser}-plugins-revoked.png`)});
  service.run(['--profile','devices','configuration','grant',receipt.id,grants().revision]);
  await panel.getByRole('button',{name:'Refresh plugin status',exact:true}).click();await panel.locator('.plugin-rows .plugin-row').first().waitFor();
  await page.getByRole('button',{name:'Close settings',exact:true}).click();assert.equal(await input.inputValue(),'Retained while inspecting plugins');await input.fill('');
}
