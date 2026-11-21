import {closeDetails} from "./controls.mjs";
import assert from "node:assert/strict";
import { png } from "./png.mjs";
import { join } from "node:path";
import { writeFile } from 'node:fs/promises';

export async function transferImage(target, type, width, height) {
  const bytes = Array.from(png(width, height, [34, 110, 160, 255]));
  return target.evaluate((node, {bytes, type}) => {
    const transfer = new DataTransfer();
    transfer.items.add(new File([new Uint8Array(bytes)], 'clipboard.png', {type:'image/png'}));
    const options = {bubbles:true, cancelable:true, composed:true};
    const event = type === 'paste'
      ? new ClipboardEvent('paste', {...options,clipboardData:transfer})
      : new DragEvent(type, {...options,dataTransfer:transfer});
    // Firefox constructs an empty clipboard for scripted ClipboardEvents.
    if (type === 'paste' && !event.clipboardData?.files.length)
      Object.defineProperty(event, 'clipboardData', {value:transfer});
    return !node.dispatchEvent(event);
  }, {bytes, type});
}

export async function verifyMarkdownImages(page, markdown, service) {
  const thumbnail=markdown.getByRole("button",{name:"View full image: Remote alt text"});
  await thumbnail.scrollIntoViewIfNeeded();
  const inspect = async image => {
    const element=await image.elementHandle();
    try {
      await page.waitForFunction(node=>node.naturalWidth>0,element);
      return await element.evaluate(node=>[node.naturalWidth,node.naturalHeight,node.referrerPolicy]);
    } finally { await element.dispose(); }
  };
  assert.deepEqual(await inspect(thumbnail.getByRole("img")),[240,100,"no-referrer"]);
  await markdown.getByText("Image preview unavailable · Missing preview",{exact:true}).waitFor();
  await thumbnail.click();
  const lightbox=page.getByRole("dialog",{name:"Image preview",exact:true});
  assert.deepEqual(await inspect(lightbox.getByRole("img")),[240,100,"no-referrer"]);
  await lightbox.getByRole("button",{name:"Close image preview"}).press("Escape");
  await lightbox.waitFor({state:"hidden"});
  assert(await thumbnail.evaluate(el=>el===document.activeElement));
  assert(service.provider.imageRequests.some(request=>request.path==="/diagram.png"));
  assert(service.provider.imageRequests.every(request=>request.referer===null));
}

export async function verifyAttachmentRail(page, pane, report) {
  const viewport = page.viewportSize();
  const rail = pane.getByRole('group', {name:'Pending attachments', exact:true});
  const picker = pane.locator('input[type=file]');
  const left = pane.getByRole('button', {name:'Scroll attachments left'});
  const right = pane.getByRole('button', {name:'Scroll attachments right'});
  try {
    await page.setViewportSize({width:390,height:844});
    for (let index = 0; index < 8; index++) {
      // The last two entries share a content identity and must keep separate controls.
      const width = 32 + Math.min(index, 6);
      await picker.setInputFiles({name:'item-' + index + '.png', mimeType:'image/png',
        buffer:png(width, 24, [32, 90, 160, 255])});
      const card = rail.getByRole('group', {name:'Image ' + (index + 1) + ' of ' + (index + 1), exact:true});
      const thumbnail = card.locator('img');
      await thumbnail.waitFor();
      assert.equal(await thumbnail.evaluate(async el => { await el.decode(); return el.naturalWidth; }), width);
    }
    await left.waitFor();
    assert.equal(await right.count(), 0);
    assert(await rail.evaluate(el => el.scrollLeft > 0 && el.lastElementChild.getBoundingClientRect().right <= el.getBoundingClientRect().right + 1));
    await page.screenshot({path:join(report, 'attachment-rail-narrow.png')});
    await left.click();
    await left.waitFor({state:'detached'});
    await right.waitFor();
    await rail.hover();
    await page.mouse.wheel(0, 48);
    await left.waitFor();
    assert(await rail.evaluate(el => el.scrollLeft > 1 && el.scrollLeft < el.scrollWidth - el.clientWidth - 1));
    await page.setViewportSize(viewport);
    await right.waitFor({state:'detached'});
    await left.waitFor({state:'detached'});
    assert(await rail.evaluate(el => el.scrollWidth === el.clientWidth));
    const remove = rail.getByRole('button', {name:'Remove image', exact:true});
    for (let index = 8; index > 0; index--) {
      await rail.getByRole('group', {name:'Image ' + index + ' of ' + index, exact:true}).waitFor();
      await remove.last().click();
    }
    await rail.waitFor({state:'detached'});
  } finally {
    await page.setViewportSize(viewport);
  }
}

export async function verifyImages(page, pane, service, report, name) {
  // setInputFiles bypasses the disabled attachment control; await the bound draft first.
  const input=await pane.locator('input[type="file"]').elementHandle();
  await page.waitForFunction(element=>!element.matches(':disabled'),input);
  await input.dispose();
  const before = service.provider.requests.length;
  const picker = pane.locator('input[type="file"]');
  const rows = pane.locator(".draft-image");
  const editor = pane.locator('textarea[aria-label$="message"]').first();
  await editor.focus();
  assert(await transferImage(editor,'paste',80,60),'the focused composer admits an image paste');
  await rows.first().waitFor();
  assert(await transferImage(pane.locator('.transcript'),'dragenter',120,80));
  const overlay = page.getByRole('status').filter({hasText:'Drag images here to add them'});
  await overlay.waitFor();
  assert(await transferImage(pane.locator('.transcript'),'drop',120,80),'the transcript admits a dropped image');
  await overlay.waitFor({state:'detached'});
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
