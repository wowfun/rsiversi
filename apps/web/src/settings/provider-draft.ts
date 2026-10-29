import type {Provider} from '../bridge.ts';

export const providerOwners = {deepseek:'rsi.ai.provider.deepseek',openai:'rsi.ai.provider.openai','openai-compatible':'rsi.ai.provider.openai-compatible'};
export const providerLabels = {deepseek:'DeepSeek',openai:'OpenAI','openai-compatible':'Custom model API'};
export type Limits = {context_window_tokens:number;default_output_reserve_tokens:number;max_output_reserve_tokens:number};
export type Row = {key:string;name:string;limits:Record<keyof Limits,string|number>};
export const newRow = (name=''):Row => ({key:crypto.randomUUID(),name,limits:{context_window_tokens:128000,default_output_reserve_tokens:4096,max_output_reserve_tokens:16384}});
export function tokenLimit(value:string|number):number {
  const match=String(value).trim().match(/^(\d+(?:\.\d+)?)\s*([km])?$/i);
  const number=match?Number(match[1])*(match[2]?.toLowerCase()==='m'?1e6:match[2]?.toLowerCase()==='k'?1e3:1):NaN;
  if(!Number.isSafeInteger(number)||number<1||number>0xffffffff)throw new Error('Token limits must be positive whole numbers; K and M are supported.');
  return number;
}
export function modelDefinitions(rows:Row[]):Record<string,Limits> {
  const models:Record<string,Limits>={};
  for(const row of rows){
    const name=row.name.trim();
    if(!name||name.length>128||/[\s\x00-\x1f\x7f]/.test(name))throw new Error('Enter a model ID without spaces, up to 128 characters.');
    if(Object.hasOwn(models,name))throw new Error(`Model ID “${name}” is repeated.`);
    const limits=Object.fromEntries(Object.entries(row.limits).map(([key,value])=>[key,tokenLimit(value)])) as Limits;
    if(limits.default_output_reserve_tokens>limits.max_output_reserve_tokens||limits.max_output_reserve_tokens>=limits.context_window_tokens)throw new Error(`For ${name}, default output must not exceed max output, and max output must be smaller than the context window.`);
    Object.defineProperty(models,name,{value:limits,enumerable:true,configurable:true,writable:true});
  }
  return models;
}
export function draftRows(entry?:Provider):Row[] {
  if(!entry)return [newRow('deepseek-flash')];
  return Object.entries((entry.config.language_models??{}) as Record<string,Limits>).map(([name,limits])=>({...newRow(name),limits:{...limits}}));
}
export function providerModelConfig(provider:Provider['provider'],models:Record<string,Limits>,original:Record<string,unknown>={}):Record<string,unknown> {
  const config:Record<string,unknown>={...original,language_models:models};
  for(const field of ['default_output_tokens','reasoning_efforts']){
    const entries=original[field];
    if(entries&&typeof entries==='object')config[field]=Object.fromEntries(Object.entries(entries).filter(([name])=>Object.hasOwn(models,name)));
  }
  if(provider==='deepseek'){
    const caps=(config.default_output_tokens??{}) as Record<string,number>;
    config.default_output_tokens=Object.fromEntries(Object.entries(models).map(([name,limits])=>[name,Math.min(Object.hasOwn(caps,name)?caps[name]:limits.default_output_reserve_tokens,limits.max_output_reserve_tokens)]));
  }
  return config;
}
export function replaceProvider(deployments:Provider[],entry:Provider,original?:string):Provider[] {
  const id=String(entry.config.deployment);
  if(deployments.some(item=>item.config.deployment===id&&item.config.deployment!==original))throw new Error('A provider with this ID already exists.');
  if(original!==undefined&&!deployments.some(item=>item.config.deployment===original))throw new Error('This provider was removed. Reopen the editor.');
  return original===undefined?[...deployments,entry]:deployments.map(item=>item.config.deployment===original?entry:item);
}
