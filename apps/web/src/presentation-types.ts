// Document types for the Rust GUI presentation; no runtime or validation owner.
import type {Model} from './bridge.ts';
import type {ComposerProjection, Media} from './session/composer-state.ts';
import type {MarkdownEvent} from './session/markdown.tsx';
import type {TokenUsage, MetricsRead} from './session/metrics.tsx';
import type {InlineCard} from './session/transcript.tsx';
export type SandboxMode = 'read-only' | 'workspace-write' | 'danger-full-access';
export type ForkSelection = 'all' | 'none' | {last:number};
export interface SessionCreation {workspace_id:string;session_id:string;agent_preset_id:string|null;fork?:{source_session_id:string;point:{kind:'before_turn'|'through_turn';turn_id:string};turns:ForkSelection}}
export interface MediaLimits {images:number;upload_bytes:number;preview_objects:number;preview_bytes:number;preview_concurrency:number}
export interface SettingsCatalog {ticket:string;namespace:string|null;page:{namespaces:string[];next:string|null}|null;error:string|null}
export interface TranscriptBlock {
  summary_seq?: string;
  compacting?: boolean;
  trigger?: 'schedule' | 'goal' | 'agent' | 'completion' | 'request';
  key: string;
  role: string;
  title: string;
  text: string;
  recall?: 'text' | 'recorded';
  images?: {media:Media;name?:string}[];
  markdown?: MarkdownEvent[];
  clipped?: boolean;
  sources: number;
  request_intent_seq?: string;
  scheduled_retry?: true;
  external?: unknown;
  created_schedule?: {id:string; title:string; interval_ms:number|null};
  presented?: (({path:string; path_hex?:never} | {path_hex:string; path?:never}) & {description:string})[];
  tool?: {
    name?: string | null;
    argument_summary?: string | null;
    arguments?: unknown;
    result?: {seq:string} | null;
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
  closing_prose?: number[];
  feedback_message?: string;
  retries?: ModelRetry[];
  terminal?: string | null;
  reading_sequence?: string;
  timestamps?: Record<string, number>;
  elapsed_ms?: number | null;
  usage?: TokenUsage | null;
  usage_routes?: Model[];
}
export interface ModelRetry {
  block: number;
  number: number;
  maximum: number | null;
  delay_ms: number;
  reason: string;
  explanation: string;
  state: 'scheduled' | 'started' | 'cancelled';
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
export interface AttachedWorkspace {key:string;coordinates:import('./bridge.ts').Coordinates;registration:{status:'registered';id:string;name:string|null}|{status:'unregistered'|'unavailable'}}
export interface BackgroundJob {
  id:string; origin:string|null; name:string;
  status:'running'|'stopping'|'completed'|'failed'|'cancelled'|'outcome_unknown';
  started_ms:number; finished_ms:number|null; output_retained:boolean;
}
export interface NativeSurface extends ComposerProjection {
  goal_armed?:boolean|null;
  kind:'native';header:string;agent_preset:string;draft?:boolean;generation:string;session:string;workspace:AttachedWorkspace;
  selection:string;creation:SessionCreation|null;ui_revision:string;active:string|null;
  completions:{query:string;sequence:string;entries:import('./session/projection-types.ts').CompletionEntry[];notice:string|null}|null;
  transcript:TranscriptView;notice?:string;historical?:boolean;history_more?:boolean;
  can_stop_work?:boolean; background_jobs?:BackgroundJob[]|null; job_notice?:string|null;
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
