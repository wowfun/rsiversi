// Document types for the Rust GUI presentation; no runtime or validation owner.
import type {ComposerProjection} from './session/composer-state.ts';
import type {MarkdownEvent} from './session/markdown.tsx';
import type {TokenUsage, MetricsRead} from './session/metrics.tsx';
import type {InlineCard} from './session/transcript.tsx';
export type SandboxMode = 'read-only' | 'workspace-write' | 'danger-full-access';
export type ForkSelection = 'all' | 'none' | {last:number};
export interface SessionCreation {workspace_id:string;session_id:string;agent_preset_id:string|null;fork?:{source_session_id:string;point:{kind:'before_turn'|'through_turn';turn_id:string};turns:ForkSelection}}
export interface MediaLimits {images:number;upload_bytes:number;preview_objects:number;preview_bytes:number;preview_concurrency:number}
export interface SettingsCatalog {ticket:string;namespace:string|null;page:{namespaces:string[];next:string|null}|null;error:string|null}
export interface TranscriptBlock {
  key: string;
  role: string;
  title: string;
  text: string;
  markdown?: MarkdownEvent[];
  clipped?: boolean;
  sources: number;
  external?: unknown;
  tool?: {
    name?: string | null;
    argument_summary?: string | null;
    arguments?: unknown;
    result?: unknown;
    rejection?: unknown;
    intent_present: boolean;
    phase: "prepared" | "running" | "rejected" | {settled: "completed" | "tool_failed" | "process_failed"};
  };
}
export interface TranscriptTurn {
  id: string;
  status: string;
  running: boolean;
  foldable: boolean;
  partial: boolean;
  blocks: string[];
  process: string[];
  candidate: string[];
  answer: string[];
  terminal?: string | null;
  timestamps?: Record<string, number>;
  elapsed_ms?: number | null;
  usage?: TokenUsage | null;
}
export interface TranscriptView {
  status?: string;
  blocks: TranscriptBlock[];
  omitted?: boolean;
  turns?: { revision?: string; entries: Record<string, TranscriptTurn> };
}
export interface ExternalSnapshot {id:string;endpoint:string;cwd:string;status:string;completion:string|null;generation:string;epoch:string}

export interface ExternalSource {conversation:string;epoch:string;sequence:string;kind:'user'|'update'|'permission'}
export interface ExternalBlock {key:string;role:string;text:string;source:ExternalSource;truncated:boolean}
export interface ExternalPermission {id:string;generation:string;title:string;options:{id:string;name:string;kind:string}[]}
export interface ExternalView {
  conversation:{kind:'external';id:string};following:boolean;more:boolean;busy:boolean;
  observed:{snapshot:ExternalSnapshot;connected:boolean;permissions:ExternalPermission[]};
  blocks:ExternalBlock[];capabilities:{goal:boolean;preset:boolean;submit:boolean;load:boolean;resume:boolean};diagnostic:string|null;
}
export interface ExternalSurface {
  kind:'external';generation:string;selection:string;external:ExternalView;
  attention_focus?:{kind:string;request:string;generation:string}|null;
  external_source?:{start:number;end:number;text:string;more:boolean;source:ExternalSource}|null;
}
export interface NativeSurface extends ComposerProjection {
  kind:'native';header:string;agent_preset:string;generation:string;session:string;path:string;workspace:string;
  selection:string;creation:SessionCreation|null;sandbox:SandboxMode;ui_revision:string;active:string|null;
  completions:{query:string;sequence:string;entries:import('./session/projection-types.ts').CompletionEntry[];notice:string|null}|null;
  transcript:TranscriptView;notice?:string;historical?:boolean;history_more?:boolean;
  metrics?:MetricsRead|null;context_capacity_tokens?:number|null;ui_cards?:boolean;inline?:Record<string,InlineCard>;
  ui_surfaces:{bundle:string;title:string;reference:{name:string;[key:string]:unknown}}[];
}
export type Surface = NativeSurface | ExternalSurface;
export interface TranscriptPatch {
  remove:string[];upsert:TranscriptBlock[];order?:string[];
  fields:Partial<Omit<TranscriptView,'blocks'|'turns'>>;
  turns?:{revision?:string;remove:string[];upsert:Record<string,TranscriptTurn>};
}
export type PresentationFrame = {kind:'snapshot';frame_id:string;view:import('./bridge.ts').View} | {
  kind:'patch';frame_id:string;base_frame_id:string;sections:Partial<Omit<import('./bridge.ts').View,'surfaces'>>;
  surfaces:{surface:string;fields:Partial<NativeSurface>|Partial<ExternalSurface>;transcript?:TranscriptPatch}[];
};
