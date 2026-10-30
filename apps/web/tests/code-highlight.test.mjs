import assert from 'node:assert/strict';
import {test} from 'node:test';
import {highlightCode} from '../code-highlight.js';

test('completed highlighting preserves exact text and supplies light/dark colors',t=>{
  // Test color correctness independently of host CPU load; the deadline has its
  // own advancing-clock case below and remains active in production.
  t.mock.method(Date,'now',()=>0);
  const text='const html = "<img onerror=alert(1)>";\nconsole.log("界");\n';
  const lines=highlightCode(text,'js');
  assert.equal(lines.map(line=>line.map(token=>token.content).join('')).join('\n'),text);
  assert(lines.flat().some(token=>token.light!==token.dark));
  assert.deepEqual(highlightCode(text,'javascript'),lines);
});

test('an exhausted tokenizer budget returns plain text, then a later call can recover',t=>{
  let now=0;
  const clock=t.mock.method(Date,'now',()=>now+=1000);
  assert.equal(highlightCode('const x = "preserve <text>";','js'),null);
  clock.mock.mockImplementation(()=>0);
  const text='const x = "preserve <text>";';
  assert.equal(highlightCode(text,'js').flat().map(token=>token.content).join(''),text);
});

test('unknown languages and oversized code avoid highlighting without trimming source',()=>{
  assert.equal(highlightCode('hello','unknown'),null);
  assert.equal(highlightCode('x'.repeat(4097),'js'),null);
  assert.equal(highlightCode(('界'.repeat(1024)+'\n').repeat(86),'js'),null);
  assert.equal(highlightCode('let x=1;\n'.repeat(5000),'js'),null,'token DOM work is bounded separately');
});

test('short lines share one document deadline across both themes',t=>{
  let now=0;
  const clock=t.mock.method(Date,'now',()=>now+=2);
  const text='const x = 1;\n'.repeat(300);
  assert.equal(highlightCode(text,'js'),null);
  clock.mock.mockImplementation(()=>0);
  assert.equal(highlightCode(text,'js').map(line=>line.map(t=>t.content).join('')).join('\n'),text);
});

test('the queue can distinguish a time slice from permanent plain-text fallback',t=>{
  let now=0,exhausted=0;
  t.mock.method(Date,'now',()=>now+=100);
  const options={budgetMs:16,onBudgetExceeded:()=>exhausted++};
  assert.equal(highlightCode('const value = 7;','js',options),null);
  assert.equal(exhausted,1);
  assert.equal(highlightCode('unsupported','unknown',options),null);
  assert.equal(highlightCode('x'.repeat(4097),'js',options),null);
  assert.equal(exhausted,1);
});
