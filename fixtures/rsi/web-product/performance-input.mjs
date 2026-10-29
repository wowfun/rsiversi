import {readFile} from 'node:fs/promises';
import {join} from 'node:path';
import {createHash} from 'node:crypto';

export async function performanceEntrypoint(source, variant) {
  const path=variant==='baseline'?'app.js':'src/main.tsx';
  const bytes=await readFile(join(source,path));
  return {bytes,identity:{path,sha256:createHash('sha256').update(bytes).digest('hex')}};
}
