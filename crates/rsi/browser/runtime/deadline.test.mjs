import {test} from 'node:test';
import assert from 'node:assert/strict';
import {deadline} from './deadline.mjs';
test('navigation predicates text and image share one finite budget',()=>{
  let elapsed=0;
  const timeout=deadline(110000,()=>elapsed);
  assert.equal(timeout(20000),20000);
  elapsed+=20000;
  for(let predicate=0;predicate<16;predicate++){
    assert.equal(timeout(5000),5000);
    elapsed+=5000;
  }
  assert.equal(timeout(20000),10000);
  elapsed+=10000;
  assert.throws(()=>timeout(20000),{name:'TimeoutError'});
});
