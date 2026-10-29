import assert from 'node:assert/strict';
import {test} from 'node:test';
import {sourceFixture} from './source-fixture.mjs';

test('concurrent source fixtures own distinct loopback listeners and retain product CSP',async()=>{
  const results=await Promise.allSettled(['first','second'].map(name=>sourceFixture({
    name:`listener-check-${name}`,source:'document.body.dataset.ready="true";',
  })));
  const fixtures=results.filter(result=>result.status==='fulfilled').map(result=>result.value);
  try {
    for(const result of results)if(result.status==='rejected')throw result.reason;
    const urls=fixtures.map(fixture=>new URL(fixture.url));
    assert.equal(new Set(urls.map(url=>url.port)).size,2);
    for(const url of urls){
      assert.equal(url.hostname,'127.0.0.1');
      const response=await fetch(url);
      assert.equal(response.status,200);
      assert.match(response.headers.get('content-security-policy'),/default-src 'self'/);
      assert.match(await response.text(),/listener-check-(first|second)\.tsx/);
    }
  } finally {await Promise.all(fixtures.map(fixture=>fixture.close()))}
});
