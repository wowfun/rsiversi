import assert from 'node:assert/strict';
import {test} from 'node:test';
import {spawn} from 'node:child_process';
import {EventEmitter} from 'node:events';
import {PassThrough} from 'node:stream';
import {holdGeneration} from './generation-lease.mjs';

function launch(source) {
  return (binary,args,options)=>{
    assert.equal(binary,process.execPath);assert.deepEqual(args,['fixture-lease','selected-bundle']);
    return spawn(process.execPath,['--input-type=module','-e',source],options);
  };
}
const environment={RSI_APP_TOOLS_BINARY:process.execPath};
test('early exit waits for stderr drain before diagnosing failure',async()=>{
  const fake=()=>{
    const child=new EventEmitter();child.stdin=new PassThrough();child.stdout=new PassThrough();child.stderr=new PassThrough();child.kill=()=>{};
    setImmediate(()=>{
      child.emit('exit',3,null);
      child.stderr.emit('data',Buffer.from('primary diagnostic after exit'));
      child.emit('close',3,null);
    });
    return child;
  };
  await assert.rejects(holdGeneration('selected-bundle',environment,fake),/primary diagnostic after exit/);
});

test('startup keeps the primary failure and forcibly reaps a helper ignoring termination',async()=>{
  let child;
  const start=launch("process.on('SIGTERM',()=>{});process.stdout.write('wrong\\n');process.stdin.resume();setInterval(()=>{},1000);");
  await assert.rejects(holdGeneration('selected-bundle',environment,(...args)=>{child=start(...args);return child;}),/Invalid fixture lease readiness/);
  assert(child.exitCode!==null || child.signalCode!==null);
});
test('fixture lease accepts exact readiness and releases through EOF',async()=>{
  const lease=await holdGeneration('selected-bundle',environment,launch("process.stdout.write('ready\\n');process.stdin.resume();"));
  await lease.close();
});

test('explicit cleanup kills and reaps a helper that ignores EOF',async()=>{
  let child;
  const start=launch("process.stdout.write('ready\\n');process.stdin.resume();setInterval(()=>{},1000);");
  const lease=await holdGeneration('selected-bundle',environment,(...args)=>{child=start(...args);return child;});
  await assert.rejects(lease.close(),/cleanup timed out/);
  assert(child.exitCode!==null || child.signalCode!==null,'helper was reaped');
});
test('fixture lease rejects missing helper, bad readiness, excess output and early exit',async()=>{
  await assert.rejects(holdGeneration('selected-bundle',{RSI_APP_TOOLS_BINARY:'/does-not-exist/rsi-helper'}),/Build the fixture lease helper/);
  for(const source of ["process.stdout.write('wrong\\n');process.stdin.resume();", "process.stdout.write('x'.repeat(33));process.stdin.resume();", "process.exit(3)"]){
    await assert.rejects(holdGeneration('selected-bundle',environment,launch(source)),/Invalid fixture lease readiness|exited early/);
  }
});
test('normal Node exit releases the supervisor without hanging; early death fails Node',async()=>{
  const module=new URL('./generation-lease.mjs',import.meta.url).href;
  for(const early of [false,true]) {
    const source=`import {holdGeneration} from ${JSON.stringify(module)};import {spawn} from 'node:child_process';
      await holdGeneration('selected-bundle',{RSI_APP_TOOLS_BINARY:process.execPath},(_binary,_args,options)=>spawn(process.execPath,['-e',${JSON.stringify(early?"process.stdout.write('ready\\n');setTimeout(()=>process.exit(3),20)":"process.stdout.write('ready\\n');process.stdin.resume();")}],options));
      ${early?'await new Promise(resolve=>setTimeout(resolve,200));':''}`;
    const child=spawn(process.execPath,['--input-type=module','-e',source],{stdio:['ignore','pipe','pipe'],windowsHide:true});
    const timer=setTimeout(()=>child.kill(),5000);let stderr='';child.stderr.on('data',value=>stderr+=value);
    const code=await new Promise(resolve=>child.once('exit',resolve));clearTimeout(timer);
    assert.equal(code,early?1:0,stderr);
    if(early)assert.match(stderr,/lease exited early/);
  }
});
