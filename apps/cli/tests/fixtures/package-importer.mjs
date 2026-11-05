import { createInterface } from 'node:readline';
import { mkdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
const generation = process.argv[2];
const pending = new Set();
const send = (id, value) => process.stdout.write(JSON.stringify({generation,message:{kind:'reply',id,result:{status:'ok',value}}})+'\n');
createInterface({input:process.stdin}).on('line', async line => {
  const {message} = JSON.parse(line);
  if(message.kind === 'cancel' && pending.delete(message.id)) {send(message.id,null);return;}
  if(message.kind !== 'request') return;
  if(message.method === 'extension.hello') {send(message.id,{protocol:1});return;}
  if(message.method !== 'package.import') {send(message.id,null);return;}
  if(message.payload.specifications[0] === 'escape') {send(message.id,{format:1,directories:['../outside']});return;}
  if(message.payload.specifications[0] === 'wait') {pending.add(message.id);await writeFile(message.payload.specifications[1],'entered');return;}
  const directory=join(message.payload.staging,'imported');
  await mkdir(directory);
  const bytes=Buffer.from('verified native importer publication');
  await writeFile(join(directory,'data.txt'),bytes);
  await writeFile(join(directory,'package.json'),JSON.stringify({format:2,name:'imported',version:'1',target:'portable',dependencies:[],requires:[],exports:{},
    files:[{path:'data.txt',bytes:bytes.length,digest:createHash('sha256').update(bytes).digest('hex'),executable:false}]}));
  send(message.id,{format:1,directories:['imported']});
});
