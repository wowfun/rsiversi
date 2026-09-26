// Explicit opt-in live service ownership, shared by product probes.
import assert from 'node:assert/strict';
import {readFile,writeFile,appendFile,mkdir,copyFile,chmod} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {startService} from './service.mjs';
import {redactEvidence} from './evidence.mjs';
import {cleanupAll} from './cleanup.mjs';
export async function liveFixture({configure}={}) {
  const report=process.env.RSI_WEB_REPORT,assets=process.env.RSI_WEB_ASSETS,model=process.env.RSI_LIVE_MODEL;
  assert(report && assets && model && process.env.RSI_LIVE_ENV_FILE,'explicit live environment, model and report required');
  assert(/^[a-zA-Z0-9_.-]{1,128}$/.test(model));
  const source=await readFile(process.env.RSI_LIVE_ENV_FILE);assert(source.length<=65536);
  const match=source.toString().match(/^\s*(?:export\s+)?DEEPSEEK_API_KEY\s*=\s*(.*?)\s*$/m);assert(match,'authorized key missing');
  const key=match[1].trim().replace(/^(["'])(.*)\1$/,'$2');assert(key.length>0);
  await mkdir(report,{recursive:false});const binary=join(report,'rsi');await copyFile(process.env.RSI_WEB_BINARY??resolve('target/debug/rsi'),binary);await chmod(binary,0o700);
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
    await writeFile(join(config,'host-profiles/fixture/host.profile.toml'),`format=1\n[[steps]]\nkind="plugin"\nid="live"\nplugin="rsi.ai.provider.deepseek"\n[steps.config]\ndeployment="live"\nendpoint="https://api.deepseek.com"\ncredential={owner="rsi.ai.provider.deepseek",slot="default"}\n[steps.config.language_models.${model}]\ncontext_window_tokens=128000\ndefault_output_reserve_tokens=4096\nmax_output_reserve_tokens=16384\n[steps.config.reasoning_efforts.${model}]\nsupported=["off","low","high","max"]\ndefault="off"\n`);
    await configure?.({config,workspace});
    const cli=join(config,'application-profiles/evidence-cli');await mkdir(cli,{recursive:true});await writeFile(join(cli,'application.profile.toml'),'format=1\n[[steps]]\nkind="plugin"\nid="connection"\nplugin="rsi.application.connection"\nconfig={host_profile="fixture"}\n[[steps]]\nkind="plugin"\nid="cli"\nplugin="rsi.application.cli"\n');
  }});
    return {service, report, model, redact, close};
  } catch(error) { await close(); throw new Error(redact(error)); }
}

export async function configureProgram({config}) {
  await appendFile(join(config, 'host-profiles/fixture/host.profile.toml'), `\n[[steps]]\nkind="plugin"\nid="program-runtime"\nplugin="rsi.agent.program.runtime"\nconfig={node=${JSON.stringify(process.execPath)}}\n`);
  const settings = JSON.parse(await readFile(join(config, 'settings.json'), 'utf8'));
  settings['rsi.agent-presets'] = {default: 'program'};
  await writeFile(join(config, 'settings.json'), JSON.stringify(settings));
  const preset = join(config, 'agent-presets/program');
  await mkdir(preset, {recursive: true});
  const profile = await readFile(resolve('plugins/rsi-agent-presets/standard/agent.profile.toml'), 'utf8');
  const disabled = 'plugin = "rsi.agent.program.tools"\nenabled = false';
  assert.equal(profile.split(disabled).length, 2, 'one standard Program contribution');
  await writeFile(join(preset, 'agent.profile.toml'), profile.replace(disabled, disabled.replace('false', 'true')));
  await copyFile(resolve('plugins/rsi-agent-presets/standard/preset.toml'), join(preset, 'preset.toml'));
}
