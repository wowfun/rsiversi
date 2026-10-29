import {closeDetails} from "./controls.mjs";
import assert from "node:assert/strict";
import { png } from "./png.mjs";
import { join } from "node:path";
import { writeFile } from 'node:fs/promises';

export async function verifyImages(page, pane, service, report, name) {
  // setInputFiles bypasses the disabled attachment control; await the bound draft first.
  await pane.getByTestId('composer-send').click({trial:true});
  const before = service.provider.requests.length;
  const picker = pane.locator('input[type="file"]');
  await picker.setInputFiles([
    { name: "red.png", mimeType: "image/png", buffer: png(80, 60, [180, 45, 60, 255]) },
    { name: "blue.png", mimeType: "image/png", buffer: png(120, 80, [25, 95, 145, 255]) },
  ]);
  const rows = pane.locator(".draft-image");
  await rows.nth(1).waitFor();
  assert.equal(service.provider.requests.length, before);
  // Both real Worker reads must reach browser HTTP dispatch before either is sent.
  // The Rust Media API fixture separately holds two reads inside the provider.
  const pattern = '**/media/read/1/read';
  const received = [];
  const deliveries = [], routeErrors = [];
  let release, entered, timer;
  const gate = new Promise(resolve => { release = resolve; });
  const both = new Promise(resolve => { entered = resolve; });
  const handler = route => {
    const delivery = (async () => {
      received.push(JSON.parse(route.request().postData()));
      if (received.length === 2) entered();
      await gate;
      await route.continue();
    })().catch(error => { routeErrors.push(error); });
    deliveries.push(delivery);
    return delivery;
  };
  await page.context().route(pattern, handler);
  try {
    await rows.first().getByRole('button', {name:'Preview image',exact:true}).click();
    await rows.last().getByRole('button', {name:'Preview image',exact:true}).click();
    await Promise.race([both,new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error('Two image reads did not enter before response release')),20_000);})]);
    assert.equal(new Set(received.map(media=>media.id)).size,2);
    await picker.setInputFiles({name:'during-preview.png',mimeType:'image/png',buffer:png(8,8,[10,140,70,255])});
    await rows.nth(2).waitFor();
    await rows.nth(2).getByRole('button',{name:'Remove image',exact:true}).click();
    await rows.nth(2).waitFor({state:'detached'});
    await writeFile(join(report,`${name}-image-parallel.json`),JSON.stringify({readsBeforeRelease:received.length,distinctImages:2,uploadWhileReadsHeld:true,barrier:'real Worker at browser HTTP request dispatch',providerBarrier:'rsi-media-api two_api_reads_reach_the_provider_before_release_and_cancellation_restores_capacity'}));
  } finally {
    clearTimeout(timer); release();
    try { await Promise.all(deliveries); }
    finally { await page.context().unroute(pattern,handler); }
    if (routeErrors.length) throw new AggregateError(routeErrors, 'Image request barrier failed');
  }
  await page.waitForFunction(() => [...document.querySelectorAll('.image-preview')].filter(image=>image.naturalWidth>0).length===2);
  const firstUrl = await page.locator('.image-preview[alt="Image 80 × 60"]').getAttribute('src');
  assert.match(firstUrl, /^blob:/);
  await page.screenshot({ path: join(report, `${name}-image-preview.png`) });
  // Each gated preview opened its own detail dialog; close both before reordering.
  await closeDetails(page);
  await closeDetails(page);
  await rows.last().getByRole("button", { name: "Move image earlier", exact: true }).click();
  await rows.first().filter({ hasText: "120 × 80" }).waitFor();
  await page.screenshot({ path: join(report, `${name}-ordered-images.png`) });
  await pane.getByRole("textbox", { name: /message$/ }).fill("Review these ordered images");
  await pane.getByTestId('composer-send').click();
  await pane.locator(".transcript").getByText("Reviewed: Review these ordered images", { exact: false }).waitFor();
  await rows.first().waitFor({ state: "detached" });
  const request = service.provider.requests.findLast(item => item.prompt.trim() === "Review these ordered images");
  assert.deepEqual(request.images.map(image => [image.width, image.height]), [[120, 80], [80, 60]]);
  const imageBlock = pane.locator(".message").filter({ has: page.locator(".message-text", { hasText: "80×60" }) }).last();
  await imageBlock.getByRole("button", { name: "Inspect sources", exact: true }).click();
  await page.locator(".source-reference").filter({ hasText: "input_image" }).click();
  await page.getByRole("button", { name: "Preview source image", exact: true }).click();
  await page.waitForFunction(() => document.querySelector(".image-preview")?.naturalWidth === 80);
  assert.equal(await page.locator(".image-preview").getAttribute("src"), firstUrl, "draft and exact durable source reuse the same canonical object URL");
  await page.screenshot({ path: join(report, `${name}-durable-image-source.png`) });
  await closeDetails(page);
  // Removing an independently imported image starts no model call.
  await picker.setInputFiles({ name: "remove.png", mimeType: "image/png", buffer: png(16, 12, [15, 120, 80, 255]) });
  await rows.first().waitFor();
  const after = service.provider.requests.length;
  await rows.first().getByRole("button", { name: "Remove image", exact: true }).click();
  await rows.first().waitFor({ state: "detached" });
  assert.equal(service.provider.requests.length, after);
}
