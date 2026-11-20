import assert from 'node:assert/strict';
import {test} from 'node:test';
import {createHighlightSession} from '../code-highlight.js';
import {createHighlighterCoreSync} from 'shiki/core';
import {createJavaScriptRegexEngine} from 'shiki/engine/javascript';
import javascript from 'shiki/langs/javascript.mjs';
import light from 'shiki/themes/github-light.mjs';
import dark from 'shiki/themes/github-dark.mjs';

test('retained grammar state matches whole-block colors across slices and CRLF',t=>{
  let time=0;const clock=t.mock.method(Date,'now',()=>time+=0.25);
  const text='/* begin\r\n'+('continued comment\r\n'.repeat(30))+'*/\r\nconst value = `multiline\r\nstring`;\r\n';
  const session=createHighlightSession(text,'js');let result,previous=0,slices=0;
  do {result=session.step(16);slices++;assert(session.progress>previous);previous=session.progress;} while(result.status==='pending');
  assert(slices>1);assert.equal(result.status,'complete');assert.equal(result.tokens.map(row=>row.map(t=>t.content).join('')).join('\n'),text);
  clock.mock.mockImplementation(()=>0);
  const oracle=createHighlighterCoreSync({themes:[light,dark],langs:[javascript],engine:createJavaScriptRegexEngine()});
  try {
    const whole=oracle.codeToTokens(text,{lang:'javascript',themes:{light:'github-light',dark:'github-dark'}}).tokens;
    const expected=whole.map((row,i)=>{
      const tokens=row.map(token=>({content:token.content,light:token.htmlStyle?.color,dark:token.htmlStyle?.['--shiki-dark']}));
      if(text.split('\n')[i].endsWith('\r'))tokens.push({content:'\r',light:undefined,dark:undefined});
      return tokens;
    });
    assert.deepEqual(result.tokens,expected);
  } finally {oracle.dispose();}
});

test('completed highlighting preserves exact text and supplies light/dark colors',t=>{
  // Test color correctness independently of host CPU load; the deadline has its
  // own advancing-clock case below and remains active in production.
  t.mock.method(Date,'now',()=>0);
  const text='const html = "<img onerror=alert(1)>";\nconsole.log("界");\n';
  const lines=createHighlightSession(text,'js').step(16).tokens;
  assert.equal(lines.map(line=>line.map(token=>token.content).join('')).join('\n'),text);
  assert(lines.flat().some(token=>token.light!==token.dark));
  assert.deepEqual(createHighlightSession(text,'javascript').step(16).tokens,lines);
});

test('unknown languages and oversized code avoid highlighting without trimming source',t=>{
  t.mock.method(Date,'now',()=>0);
  assert.equal(createHighlightSession('hello','unknown'),null);
  assert.equal(createHighlightSession('x'.repeat(4097),'js'),null);
  assert.equal(createHighlightSession(('界'.repeat(1024)+'\n').repeat(86),'js'),null);
  assert.equal(createHighlightSession('let x=1;\n'.repeat(5000),'js').step(16).status,'plain','token work is bounded independently of the clock');
});

test('short lines share one document deadline across both themes',t=>{
  let now=0;
  const clock=t.mock.method(Date,'now',()=>now+=2);
  const text='const x = 1;\n'.repeat(300);
  const session=createHighlightSession(text,'js');
  assert.equal(session.step(500).status,'pending');
  clock.mock.mockImplementation(()=>0);
  assert.equal(session.step(16).tokens.map(line=>line.map(t=>t.content).join('')).join('\n'),text);
});

test('an interrupted line falls back to plain text and releases the grammar for the next block',t=>{
  let ticks=0;
  const clock=t.mock.method(Date,'now',()=>++ticks<=4?0:1000);
  const source='const value = 7;';
  assert.equal(createHighlightSession(source,'js').step(16).status,'plain');
  clock.mock.mockImplementation(()=>0);
  assert.equal(createHighlightSession(source,'js').step(16).tokens.flat().map(token=>token.content).join(''),source);
});
