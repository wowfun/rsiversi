import assert from 'node:assert/strict';
import { cp, mkdtemp, mkdir, readFile, writeFile, rm, readdir } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { createServer } from 'node:net';

// Real product factories, binary and publication. No provider or pairing override.
export async function verifyPairing(binary, source) {
  const results = [];
  for (const variant of ['missing', 'corrupt', 'oversized', 'foreign', 'bootstrap', 'wasm', 'reclassified', 'custom-profile']) {
    const root = await mkdtemp(join(tmpdir(), 'rsi-pair-'));
    const assets = join(root, 'assets');
    const probe = createServer();
    await new Promise(resolve => probe.listen(0, '127.0.0.1', resolve));
    const port = probe.address().port;
    await new Promise(resolve => probe.close(resolve));
    const env = {PATH: process.env.PATH};
    try {
      for (const [name, leaf] of [['HOME','home'], ['XDG_CONFIG_HOME','config'], ['XDG_STATE_HOME','state'], ['XDG_CACHE_HOME','cache'], ['XDG_RUNTIME_DIR','run']]) {
        env[name] = join(root, leaf); await mkdir(env[name], {mode:0o700});
      }
      await cp(source, assets, {recursive:true});
      const path = join(assets, 'rsi-build.json');
      if (variant === 'missing') await rm(path);
      else if (variant === 'corrupt') await writeFile(path, '{');
      else if (variant === 'oversized') await writeFile(path, ' '.repeat(32769));
      else if (variant === 'bootstrap' || variant === 'custom-profile') await writeFile(join(assets, 'app.js'), 'foreign bootstrap');
      else if (variant === 'wasm') await writeFile(join(assets, 'rsi_web_bg.wasm'), 'foreign WASM');
      else {
        const receipt = JSON.parse(await readFile(path, 'utf8'));
        if (variant === 'foreign') receipt.family_sha256 = 'f'.repeat(64);
        else delete receipt.bootstrap['worker.js'];
        await writeFile(path, JSON.stringify(receipt));
      }
      let args = ['web', '--no-open', '--port', String(port), '--assets', assets];
      if (variant === 'custom-profile') {
        const profile = join(env.XDG_CONFIG_HOME, 'rsi/application-profiles/custom');
        await mkdir(profile, {recursive:true});
        await writeFile(join(profile, 'application.profile.toml'), `format = 1\n[[steps]]\nkind = "plugin"\nid = "assets"\nplugin = "rsi.web.assets"\nconfig = { directory = ${JSON.stringify(assets)} }\n[[steps]]\nkind = "plugin"\nid = "service"\nplugin = "rsi.application.service"\nconfig = { host_profile = "standard" }\n[[steps]]\nkind = "plugin"\nid = "http"\nplugin = "rsi.application.serve-web"\n`);
        args = ['--profile', 'custom', '--bind', `127.0.0.1:${port}`, '--origin', `http://127.0.0.1:${port}`, '--dev-http'];
      }
      const output = spawnSync(binary, args, {cwd:root, env, encoding:'utf8', timeout:30000, maxBuffer:65536});
      assert.ifError(output.error);
      assert.notEqual(output.status, 0, variant);
      assert.match(output.stderr, /Web pairing:/, variant);
      assert.match(output.stderr, /pnpm -C apps\/web build/, variant);
      assert(!output.stdout.includes('rsi web:') && !output.stdout.includes('"serving"'), variant);
      // No Service storage, device credentials, owner socket or listener existed.
      assert.deepEqual(await readdir(env.XDG_STATE_HOME), [], variant);
      assert.deepEqual(await readdir(env.XDG_RUNTIME_DIR), [], variant);
      await new Promise((resolve, reject) => { probe.once('error', reject); probe.listen(port, '127.0.0.1', resolve); });
      await new Promise(resolve => probe.close(resolve));
      results.push({variant, exit:output.status, stage:'asset-admission', noServiceOrCredentials:true, portUnbound:true});
    } finally { await rm(root, {recursive:true, force:true}); }
  }
  return results;
}
