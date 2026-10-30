import {IconEllipsisOutlineRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import {
  memo,
  useLayoutEffect,
  useRef,
  useSyncExternalStore,
  PureComponent,
  createRef,
} from "react";
import css from "./surface.module.css";
import chat from "./chat.module.css";
import { run, type ExternalSnapshot } from "../bridge.ts";
interface Block {
  key: string;
  role: string;
  text: string;
  source: unknown;
  truncated: boolean;
}
interface Permission {
  id: string;
  generation: string;
  title: string;
  options: { id: string; name: string; kind: string }[];
}
interface ExternalData {
  generation: string;
  external: {
    observed: {
      snapshot: ExternalSnapshot;
      connected: boolean;
      permissions: Permission[];
    };
    capabilities: { submit: boolean; resume: boolean; load: boolean };
    busy: boolean;
    more: boolean;
    following: boolean;
    blocks: Block[];
  };
  attention_focus?: { kind: string; request: string; generation: string };
  external_source?: {
    start: number;
    end: number;
    text: string;
    more: boolean;
    source: unknown;
  };
}
export interface ExternalOwner {
  index: string;
  kind: "external";
  node: HTMLElement;
  staged?: { data: ExternalData; submitKey: string };
  subscribe(listener: () => void): () => void;
  getSnapshot(): number;
  mount(elements: Record<string, HTMLElement>): void;
  unmount(): void;
  applyFrame(): void;
  action(fields: Record<string, unknown>): Promise<void>;
  control(value: Record<string, unknown>): Promise<void>;
  submit(): Promise<void>;
  composerView: {
    text: string;
    inputDisabled: boolean;
    sendDisabled: boolean;
    cancelDisabled: boolean;
    notice: string;
  };
}
const RecordRow = memo(function RecordRow({
  block,
  owner,
}: {
  block: Block;
  owner: ExternalOwner;
}) {
  return (
    <article className="external-record">
      <div className="external-record-label">{block.role}</div>
      <pre>{block.text}</pre>
      <button
        type="button"
        className="quiet"
        onClick={() =>
          void run(() =>
            owner.action({
              action: "external_source",
              source: block.source,
              start: 0,
            }),
          )
        }
      >
        {block.truncated ? "Read full source" : "Source"}
      </button>
    </article>
  );
});
// Capture tail intent before React changes rows, without moving unchanged rows.
class ObservedHistory extends PureComponent<{
  data: ExternalData;
  owner: ExternalOwner;
}> {
  root = createRef<HTMLDivElement>();
  componentDidMount() {
    this.root.current!.scrollTop = this.root.current!.scrollHeight;
  }
  getSnapshotBeforeUpdate() {
    const node = this.root.current!;
    return node.scrollHeight - node.scrollTop - node.clientHeight < 80;
  }
  componentDidUpdate(
    previous: Readonly<{ data: ExternalData; owner: ExternalOwner }>,
    _state: unknown,
    bottom: boolean,
  ) {
    if (
      bottom &&
      this.props.data.external.following &&
      previous.data.external.blocks !== this.props.data.external.blocks
    )
      this.root.current!.scrollTop = this.root.current!.scrollHeight;
  }
  render() {
    return (
      <div
        ref={this.root}
        className="transcript external-transcript"
        aria-label="Observed history"
        role="region"
        tabIndex={0}
      >
        {this.props.data.external.blocks.map((block) => (
          <RecordRow
            key={`${this.props.data.generation}:${block.key}`}
            block={block}
            owner={this.props.owner}
          />
        ))}
      </div>
    );
  }
}
export function ExternalSurface({
  owner,
  selected,
}: {
  owner: ExternalOwner;
  selected: boolean;
}) {
  useSyncExternalStore(owner.subscribe, owner.getSnapshot);
  const node = useRef<HTMLElement>(null),
    input = useRef<HTMLTextAreaElement>(null),
    permissions = useRef<HTMLDivElement>(null),
    focused = useRef("");
  const data = owner.staged?.data,
    composer = owner.composerView,
    state = data?.external,
    snapshot = state?.observed.snapshot;
  useLayoutEffect(() => {
    owner.mount({ node: node.current!, input: input.current! });
    return () => owner.unmount();
  }, [owner]);
  useLayoutEffect(() => owner.applyFrame(), [owner, owner.staged]);
  useLayoutEffect(() => {
    if (input.current && input.current.value !== composer.text)
      input.current.value = composer.text;
  }, [composer.text]);
  useLayoutEffect(() => {
    const focus = data?.attention_focus,
      key = JSON.stringify([data?.generation, focus]);
    if (focus?.kind !== "external" || focused.current === key) return;
    focused.current = key;
    const row = [
      ...permissions.current!.querySelectorAll<HTMLElement>("[data-request]"),
    ].find(
      (row) =>
        row.dataset.request === focus.request &&
        row.dataset.generation === focus.generation,
    );
    row?.scrollIntoView({ block: "nearest" });
    row?.focus();
  }, [data]);
  const control = (
    text: string,
    command: Record<string, unknown>,
    disabled = false,
    hidden = false,
  ) => (
    <button
      type="button"
      className="quiet"
      disabled={!!state?.busy || disabled}
      hidden={hidden}
      onClick={() => void run(() => owner.control(command))}
    >
      {text}
    </button>
  );
  const raw = data?.external_source;
  return (
    <section
      ref={node}
      className={`${css.scope} ${chat.scope} pane external-pane${selected ? " selected" : ""}`}
      hidden={!selected}
      aria-label="External conversation"
    >
      <header className="pane-header">
        <div className="pane-heading">
          <h2 className="pane-name">{snapshot?.endpoint} · External agent</h2>
          <details className="session-menu">
            <summary aria-label="Session tools"><IconEllipsisOutlineRegular /></summary>
            <div className="pane-tools"><p className="pane-session mono">{snapshot?.cwd} · {snapshot?.id}</p></div>
          </details>
        </div>
        <p
          className="external-status"
          role="status"
          data-connected={String(state?.observed.connected)}
        >
          {state?.observed.connected
            ? snapshot?.status
            : snapshot?.status === "closed"
              ? "Closed"
              : "Unknown · disconnected"}
          {snapshot?.completion
            ? ` · ${snapshot.completion.replaceAll("_", " ")}`
            : ""}{" "}
          · Local observations
        </p>
      </header>
      <div className="external-controls">
        {control("Refresh", { kind: "refresh" })}
        {control("History from beginning", { kind: "beginning" })}
        {control("Next history page", { kind: "next" }, !state?.more)}
        {control("Follow latest", { kind: "live" }, !!state?.following)}
        {control(
          "Resume connection",
          { kind: "reconnect", setup: "resume" },
          !!state?.observed.connected,
          !state?.capabilities.resume,
        )}
        {control(
          "Reload remote history",
          { kind: "reconnect", setup: "load" },
          !!state?.observed.connected,
          !state?.capabilities.load,
        )}
        {control("Close peer", { kind: "close" }, !state?.observed.connected)}
      </div>
      <div
        ref={permissions}
        className="external-permissions"
        aria-label="External permissions"
      >
        {state?.observed.permissions.map((pending) => (
          <section
            key={`${pending.generation}:${pending.id}`}
            className="external-permission"
            data-request={pending.id}
            data-generation={pending.generation}
            tabIndex={-1}
          >
            <h3>{pending.title}</h3>
            <div className="actions">
              {pending.options.map((option) => (
                <button
                  type="button"
                  className="quiet"
                  disabled={state.busy}
                  key={option.id}
                  onClick={() =>
                    void run(() =>
                      owner.control({
                        kind: "answer",
                        generation: pending.generation,
                        permission: pending.id,
                        option: option.id,
                      }),
                    )
                  }
                >
                  {option.name} · {option.kind.replaceAll("_", " ")}
                </button>
              ))}
            </div>
          </section>
        ))}
      </div>
      {data && <ObservedHistory data={data} owner={owner} />}
      {raw && (
        <section className="external-source">
          <h3>
            Observed source · bytes {raw.start}–{raw.end}
          </h3>
          <pre>{raw.text}</pre>
          {raw.more && (
            <button
              type="button"
              className="quiet"
              onClick={() =>
                void run(() =>
                  owner.action({
                    action: "external_source",
                    source: raw.source,
                    start: raw.end,
                  }),
                )
              }
            >
              Next source window
            </button>
          )}
        </section>
      )}
      <p className="settings-error" role="alert" hidden={!composer.notice}>
        {composer.notice}
      </p>
      <form
        className="composer external-composer"
        onSubmit={(event) => {
          event.preventDefault();
          void run(() => owner.submit());
        }}
      >
        <textarea
          ref={input}
          rows={3}
          maxLength={131072}
          placeholder="Send a message to this external agent"
          aria-label="External message"
          disabled={composer.inputDisabled}
        />
        <div className="actions">
          <button
            type="button"
            className="quiet"
            disabled={composer.cancelDisabled}
            onClick={() => void run(() => owner.control({ kind: "cancel" }))}
          >
            Cancel prompt
          </button>
          <button
            type="submit"
            className="primary"
            data-testid="composer-send"
            disabled={composer.sendDisabled}
          >
            Send ↗
          </button>
        </div>
        <p className="hint">
          {owner.staged?.submitKey === "enter"
            ? "Enter to send · Shift Enter for a new line"
            : "Ctrl / ⌘ Enter to send · Enter for a new line"}
        </p>
      </form>
    </section>
  );
}
