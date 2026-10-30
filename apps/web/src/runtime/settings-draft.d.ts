import type {SettingsFields} from './settings-fields.js';
export interface SettingsProjection {
  ticket: string; namespace: string; version: unknown; text: string;
  description: {writable: boolean; defaults: unknown; metadata: {
    applies: 'live' | 'new_session' | 'restart'; description: string;
    schema: unknown; sensitive_fields: string[][];
  }};
}
export interface SettingsDraft {
  current: SettingsProjection; fields: SettingsFields; saving: boolean;
  readonly disabled: boolean; readonly canSave: boolean; readonly reloadVisible: boolean;
  readonly awaiting: boolean; readonly error: string;
  subscribe(listener: () => void): () => void; getSnapshot(): number;
  update(editor: SettingsProjection): void; reload(): Promise<void>; save(): Promise<void>; browse(): Promise<void>; retire(): void;
}
export function createSettingsDraft(initial: SettingsProjection, command: (value: Record<string, unknown>) => Promise<unknown>): SettingsDraft;
