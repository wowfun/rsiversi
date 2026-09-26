// Shared public-browser setup; probe assertions and service lifetime stay with callers.
export async function openBrowserPage(engine, errors) {
  const browser = await engine.launch();
  try {
    const context = await browser.newContext({ignoreHTTPSErrors: true, viewport: {width: 1440, height: 980}});
    const page = await context.newPage();
    page.setDefaultTimeout(30000);
    page.on('pageerror', error => errors.push(error.message));
    return {browser, page};
  } catch (error) {
    try { await browser.close(); } catch (cleanup) { throw new AggregateError([error, cleanup], 'Browser setup and cleanup failed'); }
    throw error;
  }
}
export async function connectWorkbench(page, service, label) {
  await page.goto(service.origin);
  await page.locator('#receipt').fill(JSON.stringify(service.register(label)));
  await page.getByRole('button', {name: 'Connect', exact: true}).click();
  await page.locator('#workbench').waitFor({state: 'visible'});
}
export async function openWorkspace(page, service) {
  await page.locator('.workspace-add summary').click();
  await page.getByLabel('Server directory').fill(service.workspace);
  await page.getByRole('button', {name: 'Add workspace', exact: true}).click();
  await page.locator('#workspaces .nav-item').first().click();
  await page.getByRole('button', {name: 'Trajectory', exact: true}).click();
}
