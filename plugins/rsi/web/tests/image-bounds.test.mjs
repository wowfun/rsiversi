import test from 'node:test';
import assert from 'node:assert/strict';
import { boundedImage, boundedResource, imageBudget } from '../image-bounds.js';
import { previewDocument } from '../preview-document.mjs';
import { png } from '../../../../fixtures/rsi/web-product/images.mjs';

test('large SVG use expansion is rejected before XML parsing',()=>{
  const payload=new TextEncoder().encode('<svg xmlns="http://www.w3.org/2000/svg" width="640" height="320"><g id="loop"><use href="#loop"/></g>'+ '<use href="#loop"/>'.repeat(20000)+'</svg>');
  assert.throws(()=>boundedImage(payload,'image/svg+xml'),/SVG exceeds/);
});

test('raster gate rejects large PNG headers before decoding, including disguised MIME', () => {
  const valid = png(1, 1, [1,2,3,255]);
  assert.equal(boundedImage(valid, 'image/png'), valid);
  const oversized = Buffer.from(valid); oversized.writeUInt32BE(100000, 16); oversized.writeUInt32BE(100000, 20);
  for (const mime of ['image/png', 'image/jpeg', 'image/x-icon']) assert.throws(() => boundedImage(oversized, mime), /pixel limit/);
  for (let end = 0; end < valid.length; end++) assert.throws(() => boundedImage(valid.subarray(0,end), 'image/png'));
});

test('WebP canvas and coded frame dimensions are checked independently', () => {
  const frame = Buffer.alloc(26);frame.write('RIFF');frame.writeUInt32LE(18,4);frame.write('WEBPVP8L',8);frame.writeUInt32LE(5,16);frame[20]=0x2f;
  assert.equal(boundedImage(frame,'image/webp'),frame);
  frame.writeUInt32LE(0x0fffffff,21);assert.throws(()=>boundedImage(frame,'image/webp'),/pixel limit/);
  const extended=Buffer.alloc(44);extended.write('RIFF');extended.writeUInt32LE(36,4);extended.write('WEBPVP8X',8);extended.writeUInt32LE(10,16);extended.fill(255,24,30);frame.copy(extended,30,12);
  assert.throws(()=>boundedImage(extended,'image/webp'),/pixel limit/);
  extended[20]=2;assert.throws(()=>boundedImage(extended,'image/webp'),/Animated/);
});

test('GIF, JPEG, BMP and ICO cannot hide large decoded dimensions',()=>{
  const gif=Buffer.from('47494638396101000100800000000000ffffff2c00000000010001000002024401003b','hex');
  assert.equal(boundedImage(gif,'image/gif'),gif);
  const hugeGif=Buffer.from(gif);hugeGif.writeUInt16LE(65535,6);hugeGif.writeUInt16LE(65535,8);assert.throws(()=>boundedImage(hugeGif,'image/gif'),/pixel limit/);
  const animated=Buffer.concat([gif.subarray(0,-1),gif.subarray(19)]);assert.throws(()=>boundedImage(animated,'image/gif'),/Animated/);
  const jpeg=Buffer.from('ffd8ffc00008080001000101ffd9','hex');assert.equal(boundedImage(jpeg,'image/jpeg'),jpeg);
  const secondSof=Buffer.concat([jpeg.subarray(0,-2),jpeg.subarray(2)]);assert.throws(()=>boundedImage(secondSof,'image/jpeg'),/malformed/);
  jpeg.writeUInt16BE(65535,7);jpeg.writeUInt16BE(65535,9);assert.throws(()=>boundedImage(jpeg,'image/jpeg'),/pixel limit/);
  const bmp=Buffer.alloc(54);bmp.write('BM');bmp.writeUInt32LE(40,14);bmp.writeInt32LE(10,18);bmp.writeInt32LE(-10,22);assert.equal(boundedImage(bmp,'image/bmp'),bmp);
  bmp.writeInt32LE(100000,18);bmp.writeInt32LE(100000,22);assert.throws(()=>boundedImage(bmp,'image/bmp'),/pixel limit/);
  const payload=png(1,1,[0,0,0,255]);payload.writeUInt32BE(100000,16);payload.writeUInt32BE(100000,20);
  const ico=Buffer.alloc(22);ico.writeUInt16LE(1,2);ico.writeUInt16LE(1,4);ico.writeUInt32LE(payload.length,14);ico.writeUInt32LE(22,18);
  assert.throws(()=>boundedImage(Buffer.concat([ico,payload]),'image/x-icon'),/pixel limit/);
});

test('inline bootstrap cannot terminate its script element from a string or comment', () => {
  const document=previewDocument('globalThis.example="</ScRiPt><p>oops"; // </script>');
  assert.equal((document.match(/<\/script/gi)??[]).length,1);
  const source=document.match(/<script>([\s\S]*)<\/script>/)[1];
  const value=Function(`${source}\nreturn globalThis.example;`)();
  assert.equal(value,'</ScRiPt><p>oops');delete globalThis.example;
});

test('unknown binary formats cannot bypass the supported raster gate as plain text',()=>{
  const unknown=Buffer.from('0000001866747970617669660000000061766966','hex');
  assert.throws(()=>boundedResource(unknown,'text/plain'),/Unsupported binary/);
  const script=new TextEncoder().encode('document.body.textContent="Hello 界"');
  assert.equal(boundedResource(script,'text/javascript'),script);
  const font=Buffer.from('774f463200000000','hex');assert.equal(boundedResource(font,'font/woff2'),font);
});


test('aggregate image admission charges validated dimensions once and allows the exact boundary', () => {
  const image = png(1, 1, [1,2,3,255]);
  const large = Buffer.from(image);large.writeUInt32BE(4096,16);large.writeUInt32BE(4096,20);
  const admit = imageBudget();
  assert.throws(() => boundedImage(large.subarray(0,30), 'image/png', admit), /malformed/);
  boundedImage(large, 'image/png', admit);
  boundedImage(large, 'image/png', admit);
  assert.throws(() => boundedImage(image, 'image/png', admit), /aggregate pixel limit/);
  boundedImage(image, 'image/png', imageBudget());
});
