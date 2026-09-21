import { bindSnapshotSelector } from '../vendor/dsh/renderer/bind.ts'
import type { LeafView } from './profile-leaves.tsx'

export interface Model { deployment: string; model: string }
export interface NavigationEntry { session: string; path: string; workspace: string | null; metadata: {title: string | null; archived: boolean} }
export interface AttentionPosition {conversation:{kind:'native'|'external';id:string};epoch:string;sequence:string}
export interface AttentionEntry {position:AttentionPosition;status:'waiting'|'running'|'unknown'|'unread';targets:({kind:'native';request:{kind:'approval'|'question';turn:string;request:string}}|{kind:'external';generation:string;request:string})[]}
export interface Navigation { ticket: string; filter: {query: string; archived: boolean; workspace: string | null}; entries: NavigationEntry[]; more: boolean; scanned: number; diagnostic: string | null; attention?:{entries:AttentionEntry[];truncated:boolean};attention_notice?:string }
export interface Provider { provider: 'deepseek' | 'openai' | 'openai-compatible'; config: Record<string, unknown> }
export interface Setup { ticket: string; allowed: boolean; agent: {default_model?: Model} | null; presets: {default?: string} | null; providers: {desired_revision: string; applied_revision: string; applying: boolean; diagnostic: string | null; deployments: Provider[]} | null; credential?: {provider: string; slot: string; status: {editable: boolean; availability: {kind: string; source?: string}}}; diagnostic: string | null; receipts: {operation: string; outcome: string; message: string}[] }
export interface ExternalSnapshot {id:string;endpoint:string;cwd:string;status:string;completion:string|null;generation:string;epoch:string}
export interface ExternalView {conversation:{kind:'external';id:string};following:boolean;more:boolean;busy:boolean;observed:{snapshot:ExternalSnapshot;connected:boolean;permissions:unknown[]};blocks:unknown[];capabilities:{goal:boolean;preset:boolean;submit:boolean;load:boolean;resume:boolean};diagnostic:string|null}
export interface Surface { kind?:'native'|'external';external?:ExternalView;header:string;agent_preset:string;generation: string; session: string; path: string; transcript: {status: string; blocks: unknown[]}; ui_surfaces: {title: string; reference: unknown}[] }
export interface McpCredentialTarget {server:string;reference:{owner:string;slot:string}}
export interface McpServer {id:string;enabled:boolean;transport:'http'|'stdio';credential:{owner:string;slot:string}|null;epoch:string;last_verified_sha256:string|null;ready:boolean;error:string|null;tools:{name:string;selected:boolean}[]}
export interface McpStatus {settings_pending:boolean;fresh_ready:boolean;fresh_error:string|null;servers:McpServer[]}
export interface PluginsView {leaves?:LeafView;target:{kind:string};guidance:string[];exa_available:boolean;exa_credential:{availability:{kind:string};editable:boolean}|null;exa_notice:string|null;mcp_available:boolean;mcp:McpStatus|null;mcp_notice:string|null;mcp_credential:{target:McpCredentialTarget;availability:{kind:string};editable:boolean}|null;ticket:string;diagnostic:string|null;page:{context:{target:{kind:string};availability:string;source_digest:string|null;preset_source:string|null};desired_revision:string;observed_revision:string;health:string|null;watcher:string|null;offset:number;total:number;next_offset:number|null;plugins:{instance:string;desired_plugin:string|null;enabled:boolean;origin:string;diagnostics:string[];observed:{plugin:string;state:string}|null}[]}|null}
export interface View { external_catalog?:{endpoints:{id:string;enabled:boolean}[];conversations:ExternalSnapshot[];more:boolean};plugins?: PluginsView; application_surfaces: {title:string;reference:unknown}[]; surfaces: Record<string, Surface | null>; catalog: {workspaces: {id: string; path: string}[]; workspaces_more: boolean; models: Model[]; models_more: boolean}; navigation?: Navigation; setup?: Setup; has_remote_ui: boolean }
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
  terminal(value: unknown): Promise<string> { return actions ? actions.call("terminal", JSON.stringify(value)) as Promise<string> : Promise.reject(new Error("Application is starting")) },
  command(value: Command) { if (!actions) return Promise.reject(new Error('Application is starting')); return actions.command(value) },
  open(value: Command) { if (!actions) return Promise.reject(new Error('Application is starting')); return actions.open(value) },
  select(key: string) { actions?.select(key) },
  close(key: string) { return actions?.closeSurface(key) ?? Promise.reject(new Error('Application is starting')) },
  add(key: string) { return actions?.addSurface(key) ?? Promise.reject(new Error('Application is starting')) },
}
export function report(error: unknown) { const node = document.getElementById('notice'); if (node) {node.textContent = error instanceof Error ? error.message : String(error); node.hidden = false} }
export async function run(work: () => Promise<unknown>) { try { await work() } catch (error) { report(error) } }
