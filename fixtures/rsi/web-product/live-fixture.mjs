import {liveLimits,readDeepseekKey,deepseekProfile} from './live-limits.mjs';
import './paired-env.mjs';
// Explicit opt-in live service ownership, shared by product probes.
import assert from 'node:assert/strict';
import {writeFile,mkdir} from 'node:fs/promises';
import {join} from 'node:path';
import {startService} from './service.mjs';
import {redactEvidence} from './evidence.mjs';
import {cleanupAll} from './cleanup.mjs';
export async function liveFixture({configure}={}) {
  const report=process.env.RSI_WEB_REPORT,assets=process.env.RSI_WEB_ASSETS,model=process.env.RSI_LIVE_MODEL;
  assert(report && assets && model && process.env.RSI_LIVE_ENV_FILE,'explicit live environment, model and report required');
  const limits=liveLimits();
  assert(/^[a-zA-Z0-9_.-]{1,128}$/.test(model));
  const key=await readDeepseekKey(process.env.RSI_LIVE_ENV_FILE);
  await mkdir(report,{recursive:false});const binary=process.env.RSI_WEB_BINARY;
  let service;
  const redact=text=>String(text).replaceAll(key,'[REDACTED]');
  async function close() {
    let failure;
    try { await service?.close(); } catch (error) { failure = new Error(redact(error)); }
    await cleanupAll(() => { if (failure) throw failure; }, () => redactEvidence(report, key));
  }

  try {
  service=await startService({binary,assets,report,deepseekKey:key,configure:async({config,workspace})=>{
    await writeFile(join(config,'settings.json'),JSON.stringify({'rsi.agent':{default_model:{deployment:'live',model},default_reasoning_effort:'off'}}));
    await writeFile(join(config,'host-profiles/fixture/host.profile.toml'),deepseekProfile(model,limits));
    await configure?.({config,workspace});
    const cli=join(config,'application-profiles/evidence-cli');await mkdir(cli,{recursive:true});await writeFile(join(cli,'application.profile.toml'),'format=1\n[[steps]]\nkind="plugin"\nid="connection"\nplugin="rsi.application.connection"\nconfig={host_profile="fixture"}\n[[steps]]\nkind="plugin"\nid="cli"\nplugin="rsi.application.cli"\n');
  }});
    return {service, report, model, redact, close};
  } catch(error) { await close(); throw new Error(redact(error)); }
}

export {configureProgram} from './program-fixture.mjs';
