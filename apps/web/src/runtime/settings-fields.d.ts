export interface FieldSchema {
  enum?: unknown[]; minLength?: number; maxLength?: number;
  multipleOf?: number; minimum?: number; maximum?: number;
}
interface FieldBase {label: string; title: string; description: string; schema: FieldSchema}
export interface FieldPart {key: string; included: boolean; required: boolean; field: SettingsField}
export type SettingsField = FieldBase & (
  {kind: 'object'; parts: FieldPart[]}
  | {kind: 'boolean'; value: boolean}
  | {kind: 'enum'; value: string; schema: FieldSchema & {enum: unknown[]}}
  | {kind: 'string' | 'json' | 'number' | 'integer'; value: string}
);
export interface SettingsFields {
  text: string; jsonMode: boolean; locked: boolean; initialOversized: boolean;
  reason: string; fields?: SettingsField; readonly toggleDisabled: boolean;
  edit(field: SettingsField, value: string | boolean): void;
  include(part: FieldPart, value: boolean): void;
  editJson(text: string): void; toggle(): void; value(): string; json(): string; canSave(): boolean;
}
export function settingsFields(schema: unknown, value: unknown): SettingsField;
export function fieldsJson(field: SettingsField): string;
export function createSettingsFields(schema: unknown, original: string, disabled: boolean, changed: (edited: boolean) => void): SettingsFields;
