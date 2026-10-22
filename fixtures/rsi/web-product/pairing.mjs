import assert from 'node:assert/strict';
import { cp, mkdtemp, mkdir, readFile, writeFile, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { createServer } from 'node:net';

// Real product factories, binary and publication. No provider or pairing override.
export async function verifyPairing(binary, source) {
  const results = [];
  for (const variant of ['missing', 'corrupt', 'oversized', 'invalid-family', 'bootstrap', 'wasm', 'reclassified', 'custom-profile']) {
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
        if (variant === 'invalid-family') receipt.family_sha256 = 'invalid';
        else delete receipt.bootstrap['worker.js'];
        await writeFile(path, JSON.stringify(receipt));
      }
      let args = ['web', '--no-open', '--port', String(port), '--assets', assets];
      if (variant === 'custom-profile') {
        const profile = join(env.XDG_CONFIG_HOME, 'rsi/application-profiles/custom');
        await mkdir(profile, {recursive:true});
        await writeFile(join(profile, 'application.profile.toml'), `format = 1\n[[steps]]\nkind = "plugin"\nid = "service"\nplugin = "rsi.application.service"\nconfig = { host_profile = "standard" }\n[[steps]]\nkind = "plugin"\nid = "web"\nplugin = "rsi.application.web"\n`);
        args = ["run", 'custom', '--no-open', '--port', String(port), '--assets', assets];
      }
      const output = spawnSync(binary, args, {cwd:root, env, encoding:'utf8', timeout:30000, maxBuffer:65536});
      assert.ifError(output.error);
      assert.notEqual(output.status, 0, variant);
      assert.match(output.stderr, /HTTP listener activation failed: .*check Web assets/, variant);
      assert(!output.stdout.includes('rsi web:') && !output.stdout.includes('"serving"'), variant);
      // Asset failure may leave the independently owned shared Host running.
      const status = spawnSync(binary, ["host", "status"], {cwd:root, env, encoding:"utf8", timeout:30000});
      assert.equal(status.status, 0, status.stderr);
      await new Promise((resolve, reject) => { probe.once('error', reject); probe.listen(port, '127.0.0.1', resolve); });
      await new Promise(resolve => probe.close(resolve));
      results.push({variant, exit:output.status, stage:'asset-admission', noReadiness:true, portUnbound:true});
    } finally {
      const status = spawnSync(binary, ["host", "status"], {cwd:root, env, encoding:"utf8", timeout:30000});
      if (status.status !== 0) throw new Error(`Cannot inspect fixture Host: ${status.stderr}`);
      if (status.stdout.trim() !== "stopped") {
        const stopped = spawnSync(binary, ["host", "stop", "--force"], {cwd:root, env, encoding:"utf8", timeout:45000});
        assert.equal(stopped.status, 0, stopped.stderr);
      }
      await rm(root, {recursive:true, force:true});
    }
  }
  return results;
}
