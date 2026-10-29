export interface CompletionEntry {
  replacement: string;
  description: string;
  group: string;
  resource?: unknown;
}
export interface SessionProjections {
  protected?: boolean;
  queue?: {
    queue_slot: { id: string };
    source_kind: string;
    delivery_label: string;
    message_id: string;
    read_label: string;
    withdraw_label?: string;
    convert_label?: string;
  }[];
  pending?: { kind: string; title: string; owner: string; id: string }[];
  commands?: {
    revision: { kind: string };
    commands: { name: string; description: string; draft_safe: boolean }[];
  };
  command_receipt?: {
    command: string;
    request_id: string;
    outcome: { kind: string; revision?: string; control_seq?: string };
  };
  projections?: {
    cursor: { kind: string };
    entries: {
      producer: string;
      content: { kind: string; message?: string; value?: unknown };
    }[];
  };
  projection_notice?: string;
  ui_surfaces?: { title: string; reference: unknown }[];
  resource?: {
    value?: {
      kind: string;
      resource: { name: string; source: string };
      text: string;
    };
  };
}
export interface ProjectionActions {
  commandsExpanded?: boolean;
  action(name: string, fields?: Record<string, unknown>): Promise<unknown>;
  queueAction(slot: string, action: string): Promise<unknown>;
  selectCommand(name: string): void;
  focusInput(): void;
  pickCompletion(item: CompletionEntry): void;
  updateCompletions(refresh: boolean): void;
  queueBinding?: unknown;
  completions?: {
    entries?: CompletionEntry[];
    selected: number;
    failure?: string;
    notice?: string;
  };
}
export type Register = (node: HTMLElement | null) => void;
