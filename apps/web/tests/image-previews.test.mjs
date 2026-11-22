import test from 'node:test';
import assert from 'node:assert/strict';
import {ImagePreviews} from '../image-previews.js';

const media = (id, bytes = 8) => ({id: String(id), mime: 'image/png', bytes, width: 1, height: 1});
const source = ticket => ({kind: 'source', ticket});
const tick = () => new Promise(resolve => setImmediate(resolve));
function fixture(limits = {}) {
  const reads = [], urls = new Set(); let next = 0;
  const owner = new ImagePreviews((method, payload) => {
    assert.equal(method, 'read_image');
    const selection = JSON.parse(payload);
    return new Promise((resolve, reject) => reads.push({selection, ticket:selection.ticket, resolve, reject}));
  }, {preview_concurrency: 2, preview_objects: 8, preview_bytes: 32, ...limits}, {
    createObjectURL() { const url = `blob:${++next}`; urls.add(url); return url; },
    revokeObjectURL(url) { assert(urls.delete(url), 'each URL is released once'); },
  });
  return {owner, reads, urls};
}

test('a draft file draws without a Media read and cannot substitute for the canonical detail', async () => {
  const {owner, reads, urls} = fixture({preview_objects: 1});
  const reference = media('draft');
  const draft = {kind:'draft', pane:'main', generation:'7', media:reference};
  owner.rememberDraft(reference, new Blob(['original']));
  const thumb = owner.acquire(reference, draft);
  const original = await thumb.result;
  assert.equal(reads.length, 0);
  thumb.release();
  const detail = owner.acquire(reference, source('exact'));
  assert(!urls.has(original));
  assert.deepEqual(reads[0].selection, source('exact'));
  reads[0].resolve(new Uint8Array(reference.bytes));
  const canonical = await detail.result;
  const restored = owner.acquire(reference, draft);
  assert.equal(await restored.result, canonical);
  assert.equal(reads.length, 1);
  const fresh = fixture();
  const reloaded = fresh.owner.acquire(reference, draft);
  assert.deepEqual(fresh.reads[0].selection, draft);
  fresh.reads[0].resolve(new Uint8Array(reference.bytes));
  await reloaded.result;
  owner.dispose(); fresh.owner.dispose();
  assert.equal(urls.size + fresh.urls.size, 0);
});

test('a transcript drag borrows the same canonical file until its interest is released', async () => {
  const {owner, reads} = fixture();
  const selection = {kind:'transcript',pane:'main',generation:'2',key:'prepared:5:0:0',index:1};
  const handle = owner.acquire(media('photo', 3), selection);
  assert.deepEqual(reads[0].selection, selection);
  reads[0].resolve(Uint8Array.of(1,2,3));
  await handle.result;
  const zoom = owner.acquire(media('photo', 3), selection);
  await zoom.result;
  assert.equal(handle.file, zoom.file);
  assert.deepEqual([...new Uint8Array(await zoom.file.arrayBuffer())], [1,2,3]);
  handle.release();
  assert.equal(handle.file, undefined);
  assert.equal(zoom.file.type, 'image/png');
  owner.dispose();
  assert.equal(zoom.file, undefined);
});

test('two exact-ticket reads enter before either reply and a third waits', async () => {
  const {owner, reads, urls} = fixture();
  const a = owner.acquire(media('a'), source('a')), b = owner.acquire(media('b'), source('b'));
  const c = owner.acquire(media('c'), source('c'));
  assert.deepEqual(reads.map(read => read.ticket), ['a', 'b']);
  assert.deepEqual([owner.snapshot.active, owner.snapshot.queued, owner.snapshot.reserved], [2, 1, 16]);
  reads[0].resolve(new Uint8Array(8)); await a.result;
  assert.deepEqual(reads.map(read => read.ticket), ['a', 'b', 'c']);
  assert.equal(owner.snapshot.cached + owner.snapshot.reserved, 24);
  reads[1].resolve(new Uint8Array(8)); reads[2].resolve(new Uint8Array(8));
  await Promise.all([b.result, c.result]);
  assert.equal(owner.snapshot.active, 0);
  assert.equal(urls.size, 3); owner.dispose(); assert.equal(urls.size, 0);
});

test('published raster previews and dragged files preserve the encoded media type', async () => {
  const {owner, reads} = fixture({preview_bytes: 128});
  const bytes = Buffer.from('R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==','base64');
  const image = {...media('external',bytes.length), mime:'image/gif'};
  const handle = owner.acquire(image, source('published'));
  reads[0].resolve(Uint8Array.from(bytes));
  await handle.result;
  assert.equal(handle.file.type,'image/gif');
  assert.equal(handle.file.name,'image.gif');
  assert.deepEqual(Buffer.from(await handle.file.arrayBuffer()),bytes);
  owner.dispose();
});

test('only identical pending tickets share a read; racing tickets retain one canonical URL', async () => {
  const {owner, reads, urls} = fixture();
  const a = owner.acquire(media('same'), source('first')), same = owner.acquire(media('same'), source('first'));
  const other = owner.acquire(media('same'), source('second'));
  assert.equal(reads.length, 2);
  a.release(); assert.equal(await a.result, undefined);
  reads[0].resolve(new Uint8Array(8)); const url = await same.result;
  reads[1].resolve(new Uint8Array(8)); assert.equal(await other.result, url);
  const cached = owner.acquire(media('same'), source('third')); assert.equal(await cached.result, url);
  assert.equal(reads.length, 2); assert.equal(urls.size, 1);
  assert.deepEqual([owner.snapshot.cached, owner.snapshot.reserved], [8, 0]);
  owner.dispose();
});

test('display pins survive LRU pressure; shared cache/read bytes and object slots remain bounded', async () => {
  const {owner, reads, urls} = fixture({preview_objects: 2, preview_bytes: 16});
  const a = owner.acquire(media('a'), source('a')), b = owner.acquire(media('b'), source('b'));
  const c = owner.acquire(media('c'), source('c'));
  const refused = assert.rejects(c.result, /close another preview/);
  reads[0].resolve(new Uint8Array(8)); reads[1].resolve(new Uint8Array(8));
  const [aUrl, bUrl] = await Promise.all([a.result, b.result]); await refused;
  assert.deepEqual([...urls], [aUrl, bUrl]);
  a.release();
  const retry = owner.acquire(media('c'), source('c'));
  assert(!urls.has(aUrl)); assert(urls.has(bUrl));
  assert.deepEqual([owner.snapshot.objects, owner.snapshot.active, owner.snapshot.cached + owner.snapshot.reserved], [1, 1, 16]);
  reads[2].resolve(new Uint8Array(8)); await retry.result;
  assert.equal(urls.size, 2); owner.dispose();
});

test('queue descriptions are bounded and releasing queued interest starts no read', async () => {
  const {owner, reads} = fixture();
  const handles = Array.from({length: 10}, (_, index) => owner.acquire(media(index), source(String(index))));
  assert.equal(owner.snapshot.queued, 8);
  const overflow = owner.acquire(media(10), source('10')); await assert.rejects(overflow.result, /queue is full/);
  handles[2].release(); assert.equal(await handles[2].result, undefined);
  assert.equal(owner.snapshot.queued, 7);
  owner.dispose(); reads.forEach(read => read.resolve(new Uint8Array(8)));
  await Promise.all(handles.map(handle => handle.result)); await tick();
  assert.equal(reads.length, 2); assert.equal(owner.snapshot.reserved, 0);
});

test('closed scopes fence late results without creating URLs or losing reservations early', async () => {
  const {owner, reads, urls} = fixture();
  const a = owner.acquire(media('a'), source('a')), b = owner.acquire(media('b'), source('b'));
  owner.dispose();
  assert.equal(await a.result, undefined); assert.equal(await b.result, undefined);
  assert.equal(owner.snapshot.reserved, 16);
  reads[0].resolve(new Uint8Array(8)); reads[1].reject(new Error('closed')); await tick();
  assert.deepEqual([owner.snapshot.reserved, owner.snapshot.active, urls.size], [0, 0, 0]);
  await assert.rejects(owner.acquire(media('a'), source('a')).result, /closed/);
});

test('failure or unused results release admission and allow an exact-ticket retry', async () => {
  const {owner, reads, urls} = fixture();
  const invalid = owner.acquire(media('a'), source('a')); const rejected = assert.rejects(invalid.result, /budget/);
  reads[0].resolve(new Uint8Array(9)); await rejected;
  const retry = owner.acquire(media('a'), source('a')); retry.release();
  reads[1].resolve(new Uint8Array(8)); await tick();
  assert.deepEqual([owner.snapshot.reserved, owner.snapshot.active, urls.size], [0, 0, 0]);
  const final = owner.acquire(media('a'), source('a')); reads[2].resolve(new Uint8Array(8)); await final.result;
  assert.equal(urls.size, 1); owner.dispose();
});

test('Blob conversion failure releases both transfer and conversion admission', async () => {
  const owner = new ImagePreviews(async () => new Uint8Array(8),
    {preview_concurrency:2,preview_objects:8,preview_bytes:32},
    {createObjectURL(){throw new Error('URL unavailable');},revokeObjectURL(){assert.fail('no URL was created');}});
  await assert.rejects(owner.acquire(media('a'),source('a')).result,/URL unavailable/);
  assert.deepEqual([owner.snapshot.active,owner.snapshot.reserved,owner.snapshot.cached,owner.snapshot.converting],[0,0,0,0]);
  owner.dispose();
});
