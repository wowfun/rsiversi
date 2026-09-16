import assert from 'node:assert/strict';
import {join} from 'node:path';

export async function verifyInputDialogRetirement(page, pane, kind = "reference") {
  const [button, dialog] = kind === 'file' ? ['@ File path', 'Insert workspace file path'] : ['Reference session', 'Reference a conversation'];
  await pane.getByRole('button', {name:button, exact:true}).click();
  await page.getByRole('dialog', {name:dialog, exact:true}).waitFor();
  // Retire the connection while the modal is open, as on forced disconnect.
  await page.evaluate(() => document.querySelector('#sign-out').click());
  await page.locator('#login').waitFor({state:'visible'});
  assert.equal(await page.locator('dialog.reference-dialog[open], dialog.file-picker-dialog[open]').count(), 0);
}

export async function verifyFilePicker(page,pane,service,report,browser) {
  const input=pane.getByRole('textbox',{name:'Main message',exact:true});
  const before=service.provider.requests.length;
  await input.fill('before  after');
  assert.equal(await input.inputValue(),'before  after','typing must reach the editable draft');
  await input.evaluate(element=>{element.focus();element.setSelectionRange(7,7)});
  await pane.getByRole('button',{name:'@ File path',exact:true}).click();
  assert.equal(await input.inputValue(),'before  after','opening must retain the editable draft');
  const dialog=page.getByRole('dialog',{name:'Insert workspace file path'});
  await dialog.getByLabel('Workspace-relative file path').fill('browse');
  await dialog.getByRole('button',{name:'List directory',exact:true}).click();
  await dialog.getByRole('button',{name:'00-note.txt · preview',exact:true}).waitFor();
  await page.screenshot({path:join(report,`${browser}-file-picker-directory.png`)});
  await dialog.getByRole('button',{name:'00-note.txt · preview',exact:true}).click();
  await dialog.locator('pre').filter({hasText:'FIRST-PAGE'}).waitFor();
  assert.equal(await input.inputValue(),'before  after');
  await dialog.getByRole('button',{name:'Next page',exact:true}).click();
  await dialog.locator('pre').filter({hasText:'SECOND-PAGE'}).waitFor();
  assert.equal(await page.evaluate(()=>window.filesExecuted),undefined);
  await page.screenshot({path:join(report,`${browser}-file-picker-preview.png`)});
  await page.setViewportSize({width:420,height:860});
  await page.screenshot({path:join(report,`${browser}-file-picker-narrow.png`)});
  assert(await dialog.evaluate(element=>element.scrollWidth<=element.clientWidth+1));
  await page.setViewportSize({width:1440,height:980});
  await dialog.getByRole('button',{name:'Insert file path',exact:true}).click();
  await page.locator('dialog.file-picker-dialog').waitFor({state:'detached'});
  assert.equal(await input.inputValue(),'before @"browse/00-note.txt" after');
  assert.equal(await input.evaluate(element=>element.selectionStart),'before @"browse/00-note.txt"'.length);
  assert(await input.evaluate(element=>document.activeElement===element));
  await input.fill('keep ');await input.press('End');await input.press('@');
  await dialog.waitFor();await dialog.press('Escape');
  assert.equal(await input.inputValue(),'keep @');
  assert.equal(service.provider.requests.length,before);
  await input.fill('');
}

export async function verifyReferenceDraft(page,pane,sourcePane,report,browser) {
  const input=pane.getByRole('textbox',{name:'Main message',exact:true});
  const original=await input.inputValue();
  const source=(await sourcePane.locator('.pane-session').innerText()).split(' · ').at(-1);
  await pane.getByRole('button',{name:'Reference session',exact:true}).click();
  const dialog=page.getByRole('dialog',{name:'Reference a conversation',exact:true});
  await dialog.getByRole('textbox',{name:'Source Session ID',exact:true}).fill(source);
  await dialog.getByRole('button',{name:'Capture preview',exact:true}).click();
  await dialog.locator('pre').filter({hasText:'Review the right workspace'}).waitFor();
  assert.equal(await input.inputValue(),original);
  await page.screenshot({path:join(report,`${browser}-reference-capture.png`)});
  await dialog.getByRole('button',{name:'Add to draft',exact:true}).click();
  await pane.locator('.draft-references').filter({hasText:source}).waitFor();
  await pane.getByRole('button',{name:'Preview reference',exact:true}).click();
  const preview=page.getByRole('dialog',{name:'Frozen conversation reference',exact:true});
  await preview.locator('pre').filter({hasText:'Review the right workspace'}).waitFor();
  await page.setViewportSize({width:420,height:860});
  await page.screenshot({path:join(report,`${browser}-reference-narrow.png`)});
  assert(await preview.evaluate(element=>element.scrollWidth<=element.clientWidth+1));
  await preview.press('Escape');await page.setViewportSize({width:1440,height:980});
  await pane.getByRole('button',{name:'Remove reference',exact:true}).click();
  assert.equal(await pane.locator('.reference-row').count(),0);
  assert.equal(await input.inputValue(),original);
}

export async function verifyCompletionPointer(page,pane,service,report,browser) {
  const input=pane.getByRole('textbox',{name:'Main message',exact:true});
  const before=service.provider.requests.length;
  await input.fill('/pl');
  const option=pane.locator('.completion-option').filter({hasText:'/plan'}).first();
  await option.waitFor();
  await option.evaluate(element=>{window.heldCompletionOption=element;});
  const box=await option.boundingBox();assert(box);
  await page.mouse.move(box.x+box.width/2,box.y+box.height/2);await page.mouse.down();
  const frame=await page.evaluate(()=>window.frameEvidence.snapshots+window.frameEvidence.patches);
  await page.getByRole('button',{name:'Session commands',exact:true}).evaluate(button=>button.click());
  await page.waitForFunction(before=>window.frameEvidence.snapshots+window.frameEvidence.patches>before,frame);
  await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
  assert(await option.evaluate(element=>element===window.heldCompletionOption && element.isConnected),'unrelated product frames must preserve the pressed option');
  await page.screenshot({path:join(report,`${browser}-completion-pointer.png`)});
  await page.mouse.up();
  assert.equal((await input.inputValue()).trim(),'/plan','the pointer click must insert the selected completion');
  assert.equal(service.provider.requests.length,before);
  await input.fill('');
  await page.evaluate(()=>{delete window.heldCompletionOption;});
}
