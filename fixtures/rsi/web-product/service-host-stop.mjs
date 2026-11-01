import assert from 'node:assert/strict';
import {mkdir,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {paired} from './paired-env.mjs';
import {startService} from './service.mjs';

const report=resolve(process.env.RSI_WEB_REPORT);
await mkdir(report,{recursive:false});
let service;
try {
  service=await startService({binary:paired.binary,assets:paired.assets,report});
  service.run(['host','stop']);
  const [code,signal]=await service.waitForExit();
  assert.equal(code,1,'independent Host stop is runtime listener loss after readiness');
  assert.equal(signal,null);
  await writeFile(join(report,'result.json'),JSON.stringify({ready:true,externalHostStop:true,exitCode:code,signal,mockRequests:service.provider.requests.length},null,2));
} finally {await service?.close();}
