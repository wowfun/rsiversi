import assert from "node:assert/strict";
import { deflateSync } from "node:zlib";
import { join } from "node:path";

function png(width, height, color) {
  const chunk = (name, data) => {
    const body = Buffer.concat([Buffer.from(name), data]);
    let crc = 0xffffffff;
    for (const byte of body) { crc ^= byte; for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ ((crc & 1) ? 0xedb88320 : 0); }
    const length = Buffer.alloc(4); length.writeUInt32BE(data.length);
    const checksum = Buffer.alloc(4); checksum.writeUInt32BE((crc ^ 0xffffffff) >>> 0);
    return Buffer.concat([length, body, checksum]);
  };
  const header = Buffer.alloc(13); header.writeUInt32BE(width); header.writeUInt32BE(height, 4); header[8] = 8; header[9] = 6;
  const rows = Buffer.alloc(height * (width * 4 + 1));
  for (let y = 0; y < height; y++) for (let x = 0; x < width; x++) rows.set(color, y * (width * 4 + 1) + 1 + x * 4);
  return Buffer.concat([Buffer.from([137,80,78,71,13,10,26,10]), chunk("IHDR", header), chunk("IDAT", deflateSync(rows)), chunk("IEND", Buffer.alloc(0))]);
}

export async function verifyImages(page, pane, service, report, name) {
  const before = service.provider.requests.length;
  const picker = pane.locator('input[type="file"]');
  await picker.setInputFiles([
    { name: "red.png", mimeType: "image/png", buffer: png(80, 60, [180, 45, 60, 255]) },
    { name: "blue.png", mimeType: "image/png", buffer: png(120, 80, [25, 95, 145, 255]) },
  ]);
  const rows = pane.locator(".draft-image");
  await rows.nth(1).waitFor();
  assert.equal(service.provider.requests.length, before);
  await rows.first().getByRole("button", { name: "Preview image", exact: true }).click();
  await page.waitForFunction(() => document.querySelector(".image-preview")?.naturalWidth === 80);
  const firstUrl = await page.locator(".image-preview").getAttribute("src");
  assert.match(firstUrl, /^blob:/);
  await page.screenshot({ path: join(report, `${name}-image-preview.png`) });
  await page.getByRole("button", { name: "Close details", exact: true }).click();
  await rows.last().getByRole("button", { name: "Move image earlier", exact: true }).click();
  await rows.first().filter({ hasText: "120 × 80" }).waitFor();
  await page.screenshot({ path: join(report, `${name}-ordered-images.png`) });
  await pane.getByRole("textbox", { name: /message$/ }).fill("Review these ordered images");
  await pane.getByRole("button", { name: "Send ↗", exact: true }).click();
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
  await page.getByRole("button", { name: "Close details", exact: true }).click();
  // Removing an independently imported image starts no model call.
  await picker.setInputFiles({ name: "remove.png", mimeType: "image/png", buffer: png(16, 12, [15, 120, 80, 255]) });
  await rows.first().waitFor();
  const after = service.provider.requests.length;
  await rows.first().getByRole("button", { name: "Remove image", exact: true }).click();
  await rows.first().waitFor({ state: "detached" });
  assert.equal(service.provider.requests.length, after);
}
