// Shared semantic entry points for product fixtures; every change uses real controls.
export async function resources(page) {
  const toggle=page.getByRole('button',{name:'Toggle resources',exact:true});
  if(await toggle.getAttribute('aria-expanded')!=='true')await toggle.click();
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
  if(await tab.isVisible())await tab.click();
  else {const menu=page.locator('.conversation-menu');await menu.locator('summary').click();await page.getByLabel('Active conversation',{exact:true}).selectOption(key);await menu.locator('summary').click();}
}
