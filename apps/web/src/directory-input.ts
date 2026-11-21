import {input} from './bridge.ts';
import {presentationIdentity} from './presentation.tsx';

export async function directory<T>(request: unknown): Promise<T> {
  const reply = JSON.parse(await input.directory(request));
  if (reply.status === 'failed') throw Object.assign(new Error(reply.message), {kind:reply.failure.kind});
  return reply.value;
}

let desktop: Promise<boolean> | undefined;
presentationIdentity.subscribe(() => { desktop = undefined; });
export function hostDesktop() {
  return desktop ??= directory<boolean>({action:'desktop'}).catch(() => false);
}
