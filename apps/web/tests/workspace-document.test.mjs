import assert from 'node:assert/strict';
import {test} from 'node:test';
import {workspaceDocument} from '../src/session/workspace-document.ts';

test('document links resolve against the execution host workspace on either browser platform', () => {
  for (const [root, path, expected] of [
    ['/home/work','./report.md','report.md'],
    ['/home/work','/home/work/sub/report.md','sub/report.md'],
    ['/home/work','../work/report.md','report.md'],
    ['/home/work','../outside.txt',undefined],
    ['/home/work','/home/work-other/report.md',undefined],
    ['/home/work','//server/report.md',undefined],
    ['/','/report.md','report.md'],
    ['/home/work','C:\\work\\report.md',undefined],
    ['C:\\Work','c:\\WORK\\sub\\report.md','sub/report.md'],
    ['\\\\?\\C:\\Work','\\\\?\\C:\\Work\\report.md','report.md'],
    ['C:\\Work','..\\Work\\report.md','report.md'],
    ['C:\\Work','..\\outside.md',undefined],
    ['C:\\Work','D:\\Work\\report.md',undefined],
    ['C:\\Work','\\\\server\\files\\report.md',undefined],
    ['C:\\Work','/Work2/report.md',undefined],
  ]) assert.equal(workspaceDocument(root,path),expected,root+' / '+path);
});
