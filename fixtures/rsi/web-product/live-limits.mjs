import assert from 'node:assert/strict';
import {open} from 'node:fs/promises';

export function liveLimits(environment=process.env) {
  const limits = Object.fromEntries(['context_window_tokens','default_output_reserve_tokens','max_output_reserve_tokens'].map(field=>{
    const name=`DEEPSEEK_${field.toUpperCase()}`,raw=environment[name];
    assert(typeof raw==='string' && /^[1-9][0-9]*$/.test(raw),`${name} must be an explicit positive integer`);
    const value=Number(raw);
    assert(Number.isSafeInteger(value) && value<=0xffffffff,`${name} must fit u32`);
    return [field,value];
  }));
  assert(limits.max_output_reserve_tokens>=limits.default_output_reserve_tokens,'maximum output reserve must accommodate the default output reserve');
  return limits;
}

export async function readDeepseekKey(path) {
  const file=await open(path,'r');
  try {
    const bytes=Buffer.alloc(65537);let length=0;
    while(length<bytes.length) {
      const {bytesRead}=await file.read(bytes,length,bytes.length-length,null);
      if(!bytesRead)break;
      length+=bytesRead;
    }
    assert(length<=65536,'live environment file exceeds its bound');
    const match=bytes.subarray(0,length).toString('utf8').match(/^[ \t]*(?:export[ \t]+)?DEEPSEEK_API_KEY[ \t]*=[ \t]*(.*?)[ \t]*\r?$/m);
    assert(match,'authorized DeepSeek key is absent');
    const key=match[1].trim().replace(/^(["'])(.*)\1$/,'$2');
    assert(key.trim().length>0,'authorized DeepSeek key is empty');
    return key;
  } finally {await file.close();}
}

export function deepseekProfile(model,limits) {
  assert(typeof model==='string' && /^[a-zA-Z0-9_.-]{1,128}$/.test(model),'invalid live model identifier');
  const key=JSON.stringify(model);
  return `format=1\n[[steps]]\nkind="plugin"\nid="live"\nplugin="rsi.ai.provider.deepseek"\n[steps.config]\ndeployment="live"\nendpoint="https://api.deepseek.com"\ncredential={owner="rsi.ai.provider.deepseek",slot="default"}\n[steps.config.language_models.${key}]\ncontext_window_tokens=${limits.context_window_tokens}\ndefault_output_reserve_tokens=${limits.default_output_reserve_tokens}\nmax_output_reserve_tokens=${limits.max_output_reserve_tokens}\n[steps.config.reasoning_efforts.${key}]\nsupported=["off","low","high","max"]\ndefault="off"\n`;
}
