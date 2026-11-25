export type ComposerCommandInput = {kind:'none'} | {kind:'text';hint:string;attachments?:boolean};
export interface ComposerCommand {name:string; composer_input:ComposerCommandInput|null; description?:string; draft_safe?:boolean}
export interface CompletionEntry {
  name: string;
  replacement: string;
  description: string;
  group: string;
  resource?: unknown;
  composer_input?: ComposerCommandInput | null;
}
export type QueuedBlock =
  | {type:"text"; text:string}
  | {type:"image"; media:import("./composer-state.ts").Media; presentation?:{name?:string}}
  | {type:"file"; file:{name:string; bytes:number}}
  | {type:"reference"; reference:{preview:string}};
export interface SessionProjections {
  protected?: boolean;
  queue?: {
    queue_slot: { id: string };
    source_kind: string;
    delivery_label: string;
    editable: boolean;
    message_id: string;
    read_label: string;
    withdraw_label?: string;
    convert_label?: string;
  }[];
  pending?: { kind: string; title: string; owner: string; id: string;
    review?: import("../details/model.ts").ObservedRequest;
  }[] | null;
  commands?: {
    revision: { kind: string };
    commands: (ComposerCommand & {description:string;draft_safe:boolean})[];
  };
  command_receipt?: {
    command: string;
    request_id: string;
    outcome: { kind: string; revision?: string; control_seq?: string };
  };
  projections?: {
    cursor: {kind:'draft';revision:string;fact_seq?:never} | {kind:'durable';control_seq:string;fact_seq:string};
    generation_rejection?: string | null;
    generation_rebuilding?: boolean;
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
  generation?: string;
  commandsExpanded?: boolean;
  action(name: string, fields?: Record<string, unknown>): Promise<unknown>;
  queueAction(slot: string, action: string): Promise<unknown>;
  readQueueContent(slot:string, messageId:string): Promise<QueuedBlock[] | null>;
  acquireQueueImage(media:import("./composer-state.ts").Media, generation:string, slot:string, messageId:string, index:number): import("../details/model.ts").ImageInterest;
  selectCommand(command: ComposerCommand): Promise<void>;
  captureCommandSelection(): void;
  focusInput(): void;
  pickCompletion(item: CompletionEntry): void;
  updateCompletions(refresh: boolean): void;
  queueBinding?: {generation:string; items:NonNullable<SessionProjections["queue"]>};
  completions?: {
    entries?: CompletionEntry[];
    selected: number;
    failure?: string;
    notice?: string;
  };
}
export type Register = (node: HTMLElement | null) => void;
