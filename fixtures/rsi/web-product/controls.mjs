// Shared semantic entry points for product fixtures; every change uses real controls.
import {errors} from 'playwright';
export async function resources(page) {
  const toggle=page.getByRole('button',{name:'Toggle resources',exact:true});
  if(await toggle.getAttribute('aria-expanded')!=='true')await toggle.click();
}
export const resourceSelector='.resource-dock:not([hidden]) [data-dockkit-content]:not([aria-hidden=true]) .resource-content';
const detailSelector=`#detail[open], ${resourceSelector}`;
export function details(page) {return page.locator(detailSelector).last();}
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
  if(await page.locator('#detail[open]').isVisible()) {
    await page.getByRole('button',{name:'Close details',exact:true}).click();
    await page.locator('#detail').waitFor({state:'hidden'});
    return;
  }
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
  const filter=page.getByRole('combobox',{name:'Conversation filter',exact:true});
  if(!await filter.isVisible())await page.getByRole('button',{name:'Filter conversations',exact:true}).click();
  await filter.selectOption(value);
}
export async function detailMode(page,value) {
  const menu=page.locator('.conversation-menu');
  if(!await menu.evaluate(node=>node.open))await menu.locator('summary').click();
  await page.getByLabel('Detail level',{exact:true}).selectOption(value);
  await menu.locator('summary').click();
}
export async function externalAgents(page) {
  const menu=page.locator('.external-navigation');
  if(!await menu.evaluate(node=>node.open))await menu.locator('summary').click();
}
export async function selectSurface(page,key) {
  const tab=page.locator(`#pane-tab-${key}`);
  if(await tab.getAttribute('aria-pressed')==='true')return;
  if(await tab.isVisible())await tab.click();
  else {const menu=page.locator('.conversation-menu');await menu.locator('summary').click();await page.getByLabel('Active conversation',{exact:true}).selectOption(key);await menu.locator('summary').click();}
}
