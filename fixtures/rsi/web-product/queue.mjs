import assert from 'node:assert/strict';
import {join} from 'node:path';
import {writeFile} from 'node:fs/promises';
import {waitUntil} from './service.mjs';

// Invoked while an actual Rust Turn is held by the deterministic provider.
export async function verifyQueue(page,pane,service,report,name) {
  const composer=pane.getByRole('textbox',{name:'Main message'});
  async function send(text) {await composer.fill(text);await pane.getByTestId('composer-send').click();await waitUntil(async()=>await composer.inputValue()==='', 'queued submission receipt');}
  await send('Queue original editable input');
  await send('UI Goal hold queue survivor after Stop');
  const rows=pane.locator('.queue-row');await waitUntil(async()=>await rows.count()===2,'two pending slots');
  const initialSlot=await rows.first().getAttribute('data-slot');
  await composer.fill('Ordinary composer draft survives every queue operation');
  await rows.first().getByRole('button',{name:'Edit',exact:true}).click();
  const dialog=pane.getByRole('dialog',{name:'Edit queued input'});
  await dialog.getByRole('textbox',{name:'Queued text 1'}).fill('Queue replacement content');
  await page.screenshot({path:join(report,`${name}-queue-editor.png`)});
  await dialog.getByRole('button',{name:'Save replacement',exact:true}).click();
  await dialog.waitFor({state:'hidden'});
  await waitUntil(async()=>await rows.count()===2,'replacement retains slot count');
  assert.equal(await rows.first().getAttribute('data-slot'),initialSlot);
  await pane.locator('.message.user').filter({hasText:'Queue replacement content'}).waitFor();
  assert.equal(await pane.locator('.message.user').filter({hasText:'Queue original editable input'}).count(),0);
  assert.equal(await pane.locator('.message.user').filter({hasText:'Queue replacement content'}).count(),1);
  assert.equal(await composer.inputValue(),'Ordinary composer draft survives every queue operation');
  await rows.first().getByRole('button',{name:'Steer now',exact:true}).click();
  await rows.first().filter({hasText:'Next step'}).waitFor();
  assert.equal(await rows.first().getAttribute('data-slot'),initialSlot);
  await rows.first().getByRole('button',{name:'Withdraw',exact:true}).click();
  await waitUntil(async()=>await rows.count()===1,'withdraw removes only selected slot');
  assert.equal(await composer.inputValue(),'Ordinary composer draft survives every queue operation');
  await page.screenshot({path:join(report,`${name}-queue-before-stop.png`)});
  await pane.getByRole('button',{name:'Stop',exact:true}).click();
  await waitUntil(()=>service.provider.requests.some(request=>request.prompt==='UI Goal hold queue survivor after Stop'),'Stop preserves queued next Turn');
  await pane.locator('.transcript').getByText('Waiting for fixture release.',{exact:true}).waitFor();
  assert.equal(service.provider.requests.filter(request=>request.prompt==='Queue original editable input' || request.prompt==='Queue replacement content').length,0);
  assert.equal(await composer.inputValue(),'Ordinary composer draft survives every queue operation');
  await page.screenshot({path:join(report,`${name}-queue-after-stop.png`)});
  await pane.getByRole('button',{name:'Stop',exact:true}).click();
  await pane.locator('.pane-status').filter({hasText:'Cancelled'}).waitFor();
  await writeFile(join(report,`${name}-queue.json`),JSON.stringify({replacementKeepsSlot:true,oneUserBlock:true,convertExactDisplayedTurn:true,withdrawPendingOnly:true,stopPreservesSuccessorTurn:true,composerPreserved:true,requests:service.provider.requests.filter(request=>request.prompt.includes('queue survivor') || request.prompt.includes('Queue replacement'))},null,2));
}
