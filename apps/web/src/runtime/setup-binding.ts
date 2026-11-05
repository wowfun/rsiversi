import type {Setup} from '../bridge.ts';

type Command = Record<string, unknown>;
interface Context<Owner> {
  owner(): Owner | undefined;
  closing(owner: Owner): boolean;
  snapshot(): Setup | undefined;
  subscribe(read: () => void): () => void;
  command(owner: Owner, command: Command): Promise<unknown>;
}

/** A connection captured at editor open; each mount owns its own child scope. */
export function createSetupBinding<Owner>(context: Context<Owner>) {
  const owner = context.owner(), scopes = new Set<() => void>();
  let retired = false;
  const available = () => !retired && owner !== undefined && context.owner() === owner && !context.closing(owner);
  const failure = () => new Error('This provider editor is closed. Reopen settings to continue.');
  return {
    retire() { retired = true; for (const retire of scopes) retire(); },
    open() {
      let active = true;
      const waiters = new Set<(error: Error) => void>();
      const retire = () => { active = false; for (const reject of waiters) reject(failure()); scopes.delete(retire); };
      scopes.add(retire);
      const check = () => { if (!active || !available()) { retire(); throw failure(); } };
      return {
        retire,
        get active() { return active && available(); },
        snapshot() { check(); return context.snapshot(); },
        command(command: Command) { try { check(); return context.command(owner!, command); } catch (error) { return Promise.reject(error); } },
        waitSetup(ready: (setup: Setup) => boolean): Promise<Setup> {
          return new Promise((resolve, reject) => {
            let stop = () => {}, timer: ReturnType<typeof setTimeout> | undefined;
            const finish = (error?: Error, setup?: Setup) => {
              clearTimeout(timer); stop(); waiters.delete(fail);
              if (error) reject(error); else resolve(setup!);
            };
            const fail = (error: Error) => finish(error);
            const read = () => {
              try { check(); const setup = context.snapshot(); if (setup && ready(setup)) finish(undefined, setup); }
              catch (error) { fail(error instanceof Error ? error : new Error(String(error))); }
            };
            waiters.add(fail);
            timer = setTimeout(() => fail(new Error('Configuration saved; refresh settings to read its status.')), 10000);
            stop = context.subscribe(read); read();
          });
        },
      };
    },
  };
}
export type SetupBinding = ReturnType<typeof createSetupBinding>;
export type SetupScope = ReturnType<SetupBinding['open']>;

/** Wait for this replacement, not merely any revision different from the draft. */
export function providerApplied(setup: Setup, target: string): boolean {
  const providers = setup.providers;
  if (!providers || BigInt(providers.desired_revision) < BigInt(target)) return false;
  if (providers.desired_revision !== target) throw new Error('Provider configuration changed again. Reopen this editor to inspect it.');
  if (providers.applying) return false;
  if (providers.diagnostic || providers.applied_revision !== target) throw new Error('Provider saved, but not applied. Refresh settings to inspect its status.');
  return true;
}
