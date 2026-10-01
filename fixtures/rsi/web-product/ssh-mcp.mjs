// Public Web commands, Local scoped grants and actual target stdio discovery.
import assert from 'node:assert/strict';
import {readFile,writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {waitUntil} from './service.mjs';

export async function verifySshMcp({page,service,report,local,registration,target,checks}) {
  const panel=page.getByRole('region',{name:'SSH MCP servers',exact:true});
  await panel.getByLabel('MCP execution target',{exact:true}).selectOption(target.candidate.target);
  await panel.getByLabel('SSH MCP server name',{exact:true}).fill('target-peer');
  const read=panel.getByRole('button',{name:'Read server configuration',exact:true});
  await read.click();await panel.getByRole('status').filter({hasText:'exact target/server grant'}).waitFor();
  assert.equal(await panel.getByLabel('MCP target executable',{exact:true}).count(),0);
  const scope={principal:{kind:'device',id:registration.id},scope:{kind:'ssh_stdio',target:target.candidate.target,server:'target-peer',credentials:[]}};
  const grant=granted=>local({operation:'set_grant',request:{expected:local({operation:'grants'}).revision,scope,granted}});
  grant(true);await read.click();await panel.getByRole('status').filter({hasText:'Configuration read'}).waitFor();
  const marker=join(service.workspace,'mcp-discovery.txt');
  const source=await readFile(new URL('../../../crates/rsi-mcp/core/tests/support/stdio.py',import.meta.url),'utf8');
  const script=source.replace("mode = sys.argv[1]",`assert 'SSH_AUTH_SOCK' not in os.environ\nassert 'NOTIFY_SOCKET' not in os.environ\nwith open(sys.argv[2], 'a') as proof: proof.write('started\\n')\nmode = sys.argv[1]`);
  await panel.getByLabel('MCP target executable',{exact:true}).fill('python3');
  await panel.getByLabel('MCP target working directory',{exact:true}).fill(service.workspace);
  await panel.getByLabel('MCP arguments',{exact:true}).fill(JSON.stringify(['-u','-c',script,'modern',marker]));
  await panel.getByLabel('MCP selected tools',{exact:true}).fill('echo');
  await panel.getByLabel('Enable this server',{exact:true}).check();
  await panel.getByRole('button',{name:'Save SSH MCP server',exact:true}).click();
  await panel.getByRole('status').filter({hasText:'SSH MCP operation completed'}).waitFor();
  await panel.getByRole('button',{name:'Connect SSH MCP server',exact:true}).click();
  await waitUntil(async()=>await readFile(marker,'utf8').catch(()=>null)==='started\n','actual SSH MCP process');
  await panel.getByRole('status').filter({hasText:'SSH MCP operation completed'}).waitFor();
  assert.equal(await panel.getByRole('status').filter({hasText:'could not be applied'}).count(),0);
  await panel.scrollIntoViewIfNeeded();await page.screenshot({path:join(report,'ssh-mcp-settings.png')});
  grant(false);await read.click();await panel.getByRole('status').filter({hasText:'exact target/server grant'}).waitFor();
  assert.equal(await panel.getByLabel('MCP target executable',{exact:true}).count(),0);
  assert.equal(await readFile(marker,'utf8'),'started\n','denial must not restart target');
  grant(true);await read.click();await panel.getByRole('status').filter({hasText:'Configuration read'}).waitFor();
  await panel.getByRole('button',{name:'Remove SSH MCP server',exact:true}).click();
  await panel.getByRole('status').filter({hasText:'SSH MCP operation completed'}).waitFor();
  await waitUntil(()=>panel.getByRole('button',{name:'Connect SSH MCP server',exact:true}).isDisabled(),'removed exact server');
  checks.push('SSH MCP Settings requires exact grant, saves target configuration, starts one real stdio server, rejects revoked read and removes explicitly');
}
