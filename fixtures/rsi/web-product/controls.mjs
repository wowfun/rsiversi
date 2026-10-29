// Shared semantic entry points for product fixtures; every change uses real controls.
export async function openSettings(page) {
  const trigger=page.locator('#settings-open');
  if(!await trigger.isVisible())await page.locator('.application-options > summary').click();
  await trigger.click();
}
import {errors} from 'playwright';
import {assertNoNotices} from './task-checks.mjs';
export async function navigationOptions(page) {
  const summary=page.locator('.navigation-options > summary');
  if(!await summary.evaluate(node=>node.parentElement.open))await summary.click();
}
export async function workspaceForm(page) {
  await page.getByRole('button',{name:'Choose workspace',exact:true}).click();
  const dialog=page.getByRole('dialog',{name:'Select workspace directory',exact:true});
  await dialog.locator('.directory-columns[aria-busy=false]').waitFor();
  const edit=dialog.getByRole('button',{name:'Edit directory path',exact:true});
  if(await edit.isVisible())await edit.click();
  await dialog.getByRole('textbox',{name:'Directory path',exact:true}).waitFor();
}
export async function addWorkspace(page,path) {
  await workspaceForm(page);
  const dialog=page.getByRole('dialog',{name:'Select workspace directory',exact:true});
  await dialog.getByRole('textbox',{name:'Directory path',exact:true}).fill(path);
  await dialog.locator('footer').getByRole('button',{name:'Open',exact:true}).click();
  await dialog.waitFor({state:'detached'});
}
export async function addModelProvider(page,{id,endpoint,model,provider='openai-compatible',secret,limits}) {
  await page.getByRole('button',{name:'Add model provider',exact:true}).click();
  const editor=page.getByRole('form',{name:'Add model provider',exact:true});
  if(provider==='openai-compatible') {
    await editor.getByRole('button',{name:'Custom model API',exact:true}).click();
    await editor.getByRole('textbox',{name:'Provider ID',exact:true}).first().fill(id);
    await editor.getByRole('textbox',{name:'Base URL',exact:true}).fill(endpoint);
  }
  if(secret)await editor.getByRole('textbox',{name:'API key',exact:true}).fill(secret);
  await editor.getByText('Customized settings',{exact:true}).click();
  if(provider==='deepseek') {
    await editor.getByRole('textbox',{name:'Base URL',exact:true}).fill(endpoint);
    await editor.getByRole('combobox',{name:'API protocol',exact:true}).selectOption('chat-completions');
  } else await editor.getByRole('textbox',{name:'Request path',exact:true}).fill('/v1/chat/completions');
  await editor.getByText('Connection identifiers',{exact:true}).click();
  await editor.locator('details details').getByRole('textbox',{name:'Provider ID',exact:true}).fill(id);
  // The deterministic and opt-in environment credentials are installed in this slot.
  if(!secret)await editor.getByRole('textbox',{name:'Credential slot',exact:true}).fill('default');
  if(!await editor.getByRole('textbox',{name:'Model ID 1',exact:true}).count())await editor.getByRole('button',{name:'Add model',exact:true}).click();
  await editor.getByRole('textbox',{name:'Model ID 1',exact:true}).fill(model);
  if(limits) {
    await editor.getByRole('button',{name:'Model settings 1',exact:true}).click();
    for(const [field,label] of Object.entries({context_window_tokens:'Context window',default_output_reserve_tokens:'Default output tokens',max_output_reserve_tokens:'Max output tokens'})) {
      if(limits[field]!=null)await editor.getByLabel(`${label} 1`,{exact:true}).fill(String(limits[field]));
    }
  }
  await editor.getByRole('button',{name:'Create provider',exact:true}).click();
  await editor.waitFor({state:'detached',timeout:60000});
  const selected=page.getByRole('combobox',{name:'Default model',exact:true});
  await selected.waitFor();
  if(await selected.inputValue()!==JSON.stringify({deployment:id,model}))await selected.selectOption({label:`${model} · ${id}`});
  await page.waitForFunction(label=>{const select=document.querySelector('[aria-label="Default model"]');return select&&!select.disabled&&select.selectedOptions[0]?.textContent===label;},`${model} · ${id}`);
  await assertNoNotices(page);
}
export async function navigationMode(page,value) {
  await navigationOptions(page);
  await page.getByRole('menuitemradio',{name:({flat:'Single list',workspace:'By workspace',workspace_tree:'By workspace tree',manual:'Manual',updated:'Recently updated'})[value],exact:true}).click();
}
export async function resources(page) {
  const toggle=page.getByRole('button',{name:'Toggle resources',exact:true});
  if(await toggle.getAttribute('aria-expanded')!=='true')await toggle.click();
}
export const resourceSelector='.resource-dock:not([hidden]) [data-dockkit-content]:not([aria-hidden=true]) .resource-content';
const detailSelector=resourceSelector;
export function details(page) {return page.locator(detailSelector).last();}
export async function sourceInspectionGeometry(page,{timeout=30_000}={}) {
  const result=await page.waitForFunction(selector=>{
    const dialog=[...document.querySelectorAll(selector)].at(-1);
    const heading=dialog?.closest('[data-dockkit-content]')?.querySelector('[data-dockkit-strip]');
    const controls=dialog?.querySelector('.block-sources > .actions');
    if(!dialog?.isConnected||!heading||!controls||!dialog.getClientRects().length)return null;
    const bounds=dialog.getBoundingClientRect();
    return {headingBottom:heading.getBoundingClientRect().bottom,contentTop:bounds.top,controlsBottom:controls.getBoundingClientRect().bottom,viewportHeight:innerHeight,scrollTop:dialog.scrollTop};
  },resourceSelector,{timeout});
  try{return await result.jsonValue();}finally{await result.dispose();}
}
function readyUiControl({rootSelector,label,dispatch=false,deadline}) {
  if(Date.now()>=deadline)return false;
  const root=[...document.querySelectorAll(rootSelector)].at(-1);
  const button=root&&[...root.querySelectorAll('button')].find(button=>
    (button.getAttribute('aria-label')??button.textContent).trim()===label&&
    button.getBoundingClientRect().width>0&&button.getBoundingClientRect().height>0);
  if(!button||button.matches(':disabled'))return false;
  button.scrollIntoView({block:'center'});
  const rect=button.getBoundingClientRect();
  if(!button.contains(document.elementFromPoint(rect.x+rect.width/2,rect.y+rect.height/2)))return false;
  if(dispatch)button.click();
  return true;
}
export async function clickUiControl(page, rootSelector, label, {timeout=30_000}={}) {
  const deadline=performance.now()+timeout;
  const input={rootSelector,label,deadline:Date.now()+timeout};
  const expired=()=>new errors.TimeoutError(`Timeout ${timeout}ms exceeded waiting for ${label}`);
  for(;;) {
    const remaining=deadline-performance.now();
    if(remaining<=0)throw expired();
    const ready=await page.waitForFunction(readyUiControl,input,{timeout:Math.ceil(remaining)});
    await ready.dispose();
    if(performance.now()>=deadline)throw expired();
    if(await page.evaluate(readyUiControl,{...input,dispatch:true}))return;
  }
}
export function clickDetails(page,label) {return clickUiControl(page,detailSelector,label);}
export async function closeDetails(page) {
  const floating=page.locator('[data-dockkit-float-active=true]:visible [data-dockkit-float-close]');
  if(await floating.count()) {const owner=floating.locator('xpath=ancestor::*[@data-dockkit-content]');const id=await owner.getAttribute('data-dockkit-content');await floating.click();await page.locator(`[data-dockkit-content="${id}"]`).waitFor({state:'detached'});return;}
  const active=page.locator('.resource-dock:not([hidden]) [data-dockkit-pane-active=true] [data-dockkit-tab][aria-selected=true]:visible');
  const tab=await active.count()?active:page.locator('[data-dockkit-tab][aria-selected=true]:visible').last();
  const closing=await tab.elementHandle();
  await tab.click({button:'right'});
  await page.getByRole('menuitem',{name:'Close resource tab',exact:true}).click();
  await page.waitForFunction(element=>!element.isConnected,closing);
  await closing.dispose();
}
export async function openResource(page,name) {
  await resources(page);
  const button=page.getByRole('button',{name,exact:true});
  if(!await button.isVisible()) {
    const count=await page.locator('[data-dockkit-tab]').count();
    await page.getByRole('button',{name:'Open resources',exact:true}).first().click();
    await page.waitForFunction(count=>document.querySelectorAll('[data-dockkit-tab]').length===count+1,count);
  }
  await button.click();
}
export async function navigationFilter(page,value) {
  await navigationOptions(page);
  await page.getByRole('menuitemradio',{name:({active:'Hide archived',all:'All conversations',archived:'Archived only',attention:'Needs attention'})[value],exact:true}).click();
}
export async function detailMode(page,value) {
  const menu=page.locator('.conversation-menu');
  if(!await menu.evaluate(node=>node.open))await menu.locator('summary').click();
  await page.getByLabel('Detail level',{exact:true}).selectOption(value);
  if(await menu.evaluate(node=>node.open))await menu.locator('summary').click();
}
export async function externalAgents(page) {
  const menu=page.locator('.external-navigation');
  if(!await menu.evaluate(node=>node.open))await menu.locator('summary').click();
}
export async function sessionTools(page) {
  const menu=page.locator('.pane.selected .session-menu');
  if(!await menu.evaluate(node=>node.open))await menu.getByLabel('Session tools',{exact:true}).click();
}
export async function selectSurface(page,key) {
  const tab=page.locator(`#pane-tab-${key}`);
  if(await tab.getAttribute('aria-pressed')==='true')return;
  if(await tab.isVisible())await tab.click();
  else {const menu=page.locator('.conversation-menu');await menu.locator('summary').click();await page.getByLabel('Active conversation',{exact:true}).selectOption(key);await menu.locator('summary').click();}
}

export async function disconnect(page) {
  await page.getByRole('button',{name:'Commands',exact:true}).click();
  const search=page.getByRole('combobox',{name:'Search commands',exact:true});
  await search.fill('Disconnect');await search.press('Enter');
}
