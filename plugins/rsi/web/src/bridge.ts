import { bindSnapshotSelector } from '../vendor/dsh/renderer/bind.ts'

export interface Model { deployment: string; model: string }
export interface NavigationEntry { session: string; path: string; workspace: string | null; metadata: {title: string | null; archived: boolean} }
export interface Navigation { ticket: string; filter: {query: string; archived: boolean; workspace: string | null}; entries: NavigationEntry[]; more: boolean; scanned: number; diagnostic: string | null }
export interface Provider { provider: 'deepseek' | 'openai' | 'openai-compatible'; config: Record<string, unknown> }
export interface Setup { ticket: string; allowed: boolean; agent: {default_model?: Model} | null; presets: {default?: string} | null; providers: {desired_revision: string; applied_revision: string; applying: boolean; diagnostic: string | null; deployments: Provider[]} | null; credential?: {provider: string; slot: string; status: {editable: boolean; availability: {kind: string; source?: string}}}; diagnostic: string | null; receipts: {operation: string; outcome: string; message: string}[] }
export interface Surface { generation: string; session: string; path: string; transcript: {status: string; blocks: unknown[]}; ui_surfaces: {title: string; reference: unknown}[] }
export interface View { application_surfaces: {title:string;reference:unknown}[]; surfaces: Record<string, Surface | null>; catalog: {workspaces: {id: string; path: string}[]; workspaces_more: boolean; models: Model[]; models_more: boolean}; navigation?: Navigation; setup?: Setup; has_remote_ui: boolean }
export function observable<T>(initial: T) {
  let value = initial
  const listeners = new Set<() => void>()
  return { getSnapshot: () => value, subscribe(fn: () => void) { listeners.add(fn); return () => {listeners.delete(fn)} }, set(next: T) { if (Object.is(value, next)) return; value = next; for (const fn of listeners) fn() } }
}
export const source = observable<View | undefined>(undefined)
export const selection = observable('main')
export const useView = bindSnapshotSelector(source)
export const useSelected = bindSnapshotSelector(selection)
export const publish = source.set
export const selectSurface = selection.set
type Command = Record<string, unknown>
interface Actions { command(command: Command): Promise<unknown>; open(command: Command): Promise<unknown>; select(key: string): void; closeSurface(key: string): Promise<void>; addSurface(key: string): Promise<void>; call(method: string, payload: unknown): Promise<unknown> }
let actions: Actions | undefined
export function installActions(value: Actions) { actions = value }
export const input = {
  command(value: Command) { if (!actions) return Promise.reject(new Error('Application is starting')); return actions.command(value) },
  open(value: Command) { if (!actions) return Promise.reject(new Error('Application is starting')); return actions.open(value) },
  select(key: string) { actions?.select(key) },
  close(key: string) { return actions?.closeSurface(key) ?? Promise.reject(new Error('Application is starting')) },
  add(key: string) { return actions?.addSurface(key) ?? Promise.reject(new Error('Application is starting')) },
}
export function report(error: unknown) { const node = document.getElementById('notice'); if (node) {node.textContent = error instanceof Error ? error.message : String(error); node.hidden = false} }
export async function run(work: () => Promise<unknown>) { try { await work() } catch (error) { report(error) } }
