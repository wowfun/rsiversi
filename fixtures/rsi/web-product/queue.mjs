import assert from 'node:assert/strict';
import {join} from 'node:path';
import {writeFile} from 'node:fs/promises';
import {waitUntil} from './service.mjs';

// Invoked while an actual Rust Turn is held by the deterministic provider.
export async function verifyQueue(page,pane,service,report,name) {
  const composer=pane.getByRole('textbox',{name:'Main message'});
  async function send(text) {await composer.fill(text);await pane.getByTestId('composer-send').click();await waitUntil(async()=>await composer.inputValue()==='', 'queued submission receipt');}
  const rows=pane.locator('.queue-row');
  const count=pane.getByRole('button',{name:'2 queued messages',exact:true});
  await send('Queue original editable input');
  await rows.first().waitFor();
  await rows.first().getByText('Queue original editable input',{exact:true}).waitFor();
  assert.equal(await count.count(),0);
  await send('UI Goal hold queue survivor after Stop');
  await count.waitFor();
  assert.equal(await count.getAttribute('aria-expanded'),'false');
  assert.equal(await rows.count(),0);
  await page.screenshot({path:join(report,`${name}-queue-collapsed.png`)});
  await count.focus();
  await count.press('Enter');
  await waitUntil(async()=>await rows.count()===2,'two pending slots');
  assert.equal(await count.getAttribute('aria-expanded'),'true');
  await count.press('Space');
  await waitUntil(async()=>await rows.count()===0,'collapse unmounts rows');
  await count.press('Space');
  await rows.first().waitFor();
  const initialSlot=await rows.first().getAttribute('data-slot');
  await rows.first().getByText('Queue original editable input',{exact:true}).waitFor();
  await rows.last().getByText('UI Goal hold queue survivor after Stop',{exact:true}).waitFor();
  await composer.fill('Ordinary composer draft survives every queue operation');
  const edit=rows.first().getByRole('button',{name:'Edit queued message',exact:true});
  await edit.click();
  const field=rows.first().getByRole('textbox',{name:'Edit queued message'});
  const save=rows.first().getByRole('button',{name:'Save queued message'});
  await field.waitFor();
  assert.equal(await field.inputValue(),'Queue original editable input');
  assert.equal(await field.evaluate(node=>node===document.activeElement),true);
  assert.equal(await count.isDisabled(),true);
  await field.fill('   ');
  assert.equal(await save.isDisabled(),true);
  await field.press('Enter');
  await field.fill('Queue replacement');
  await field.press('End');
  await field.press('Shift+Enter');
  await field.pressSequentially('content');
  assert.equal(await field.inputValue(),'Queue replacement\ncontent');
  await field.dispatchEvent('keydown',{key:'Enter',isComposing:true});
  assert.equal(await field.isVisible(),true);
  // Edit and Save occupy the same action position. Exercise an actual pointer entry.
  await field.hover();
  await save.hover();
  const tip=page.getByRole('tooltip',{name:'Save queued message',exact:true});
  await tip.waitFor();
  await waitUntil(()=>tip.evaluate(bubble=>{
    const box=bubble.getBoundingClientRect(),previous=bubble.style.pointerEvents;
    bubble.style.pointerEvents='auto';
    try {
      return [box.top+box.height/2,box.bottom-1].every(y=>
        bubble.contains(document.elementFromPoint(box.left+box.width/2,y)));
    } finally {bubble.style.pointerEvents=previous;}
  }),'queue tooltip visible above composer');
  await page.screenshot({path:join(report,`${name}-queue-editor.png`)});
  await field.press('Enter');
  await field.waitFor({state:'hidden'});
  await waitUntil(async()=>await rows.count()===2,'replacement retains slot count');
  assert.equal(await rows.first().getAttribute('data-slot'),initialSlot);
  await rows.first().getByText('Queue replacement content',{exact:true}).waitFor();
  await edit.click();
  assert.equal(await field.inputValue(),'Queue replacement\ncontent');
  await field.fill('Discard this edit');
  await field.press('Escape');
  await field.waitFor({state:'hidden'});
  await waitUntil(()=>edit.evaluate(node=>document.activeElement===node),'cancel returns focus to the edit button');
  assert.equal(await pane.locator('.message.user').filter({hasText:'Queue original editable input'}).count(),0);
  assert.equal(await pane.locator('.message.user').filter({hasText:'Queue replacement content'}).count(),0);
  assert.equal(await composer.inputValue(),'Ordinary composer draft survives every queue operation');
  assert.equal(await rows.first().getByRole('button',{name:'Steer now',exact:true}).isEnabled(),true);
  await rows.first().getByRole('button',{name:'Remove queued message',exact:true}).click();
  await waitUntil(async()=>await rows.count()===1,'withdraw removes only selected slot');
  assert.equal(await count.count(),0);
  assert.equal(await composer.inputValue(),'Ordinary composer draft survives every queue operation');
  await page.screenshot({path:join(report,`${name}-queue-before-stop.png`)});
  await pane.getByRole('button',{name:'Stop',exact:true}).click();
  await waitUntil(()=>service.provider.requests.some(request=>request.prompt==='UI Goal hold queue survivor after Stop'),'Stop preserves queued next Turn');
  await pane.locator('.transcript').getByText('Waiting for fixture release.',{exact:true}).waitFor();
  assert.equal(await pane.locator('.message.user').filter({hasText:'UI Goal hold queue survivor after Stop'}).count(),1);
  assert.equal(service.provider.requests.filter(request=>request.prompt==='Queue original editable input' || request.prompt==='Queue replacement\ncontent').length,0);
  assert.equal(await composer.inputValue(),'Ordinary composer draft survives every queue operation');
  await page.screenshot({path:join(report,`${name}-queue-after-stop.png`)});
  await pane.getByRole('button',{name:'Stop',exact:true}).click();
  await pane.locator('.pane-status').filter({hasText:'Cancelled'}).waitFor({state:'attached'});
  await writeFile(join(report,`${name}-queue.json`),JSON.stringify({singleRowVisible:true,multipleCollapsed:true,keyboardToggle:true,inlineEdit:true,imeAndNewline:true,cancelRestoresFocus:true,replacementKeepsSlot:true,pendingOnlyInQueue:true,replacementReadback:true,sessionDefaultsPermitSteering:true,withdrawPendingOnly:true,stopPreservesSuccessorTurn:true,composerPreserved:true,requests:service.provider.requests.filter(request=>request.prompt.includes('queue survivor') || request.prompt.includes('Queue replacement'))},null,2));
}
