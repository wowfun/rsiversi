import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
const root = fileURLToPath(new URL('../../', import.meta.url));
const [operation, ...args] = process.argv.slice(2);
if (!['dev', 'build'].includes(operation)) throw new Error('Expected dev or build');
const windows = process.platform === 'win32';
const program = windows ? 'powershell.exe' : operation === 'dev' ? 'bash' : 'cargo';
const command = operation === 'dev' ? windows ? ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', 'scripts/rsiversi.ps1', 'dev', 'web', ...args] : ['scripts/rsiversi.sh', 'dev', 'web', ...args]
  : windows ? ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', 'scripts/rsiversi.ps1', 'build', 'web', ...args]
  : ['run', '--locked', '-p', 'rsi-app-tools', '--', 'dist', 'web', ...args];
const environment = operation === 'dev' ? {...process.env, RSI_DEV_CALLER_CWD: process.env.INIT_CWD ?? root} : process.env;
const child = spawn(program, command, { cwd: root, env: environment, stdio: 'inherit' });
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => { if (!windows) child.kill(signal); });
child.on('error', error => { console.error(error.message); process.exitCode = 1; });
child.on('exit', (code, signal) => { process.exitCode = code ?? (signal ? 130 : 1); });
