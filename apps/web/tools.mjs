import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
const root = fileURLToPath(new URL('../../', import.meta.url));
const [operation, ...args] = process.argv.slice(2);
if (!['dev', 'build'].includes(operation)) throw new Error('Expected dev or build');
const windows = process.platform === 'win32';
const program = operation === 'build' && windows ? 'powershell.exe' : 'cargo';
const command = operation === 'dev' ? ['run', '--locked', '-p', 'rsi-app-tools', '--', 'dev', 'web', ...args]
  : windows ? ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', 'rsi.ps1', 'build', ...args]
  : ['run', '--locked', '-p', 'rsi-app-tools', '--', 'dist', 'web', ...args];
const child = spawn(program, command, { cwd: root, stdio: 'inherit' });
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => child.kill(signal));
child.on('error', error => { console.error(error.message); process.exitCode = 1; });
child.on('exit', (code, signal) => { process.exitCode = code ?? (signal ? 130 : 1); });
