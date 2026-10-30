import css from "./surface.module.css";
import { useRef } from "react";
import { useOwnedDialog } from "./use-owned-dialog.ts";
interface Scope {
  workspace: string;
  conversation: { kind: string; id: string };
}
interface Hit {
  preview: string;
  original: { record: { kind: string; sequence: string }; end: number };
}
interface Match {
  scope: Scope;
  hit: Hit;
  label: string;
  reference_allowed: boolean;
}
interface Coverage {
  indexed_through: string;
  observed_through: string;
  omissions: number;
  has_more: boolean;
}
type QueryScope = { kind: string; workspace?: string; source?: Scope };
type Body =
  | {
      kind: "matches";
      matches: Match[];
      next?: unknown;
      scope: QueryScope;
      query: string;
    }
  | {
      kind: "original";
      scope: Scope;
      hit: Hit;
      referenceAllowed: boolean;
      text: string;
      offset: number;
      next_offset: number;
      has_more: boolean;
    }
  | { kind: "frozen"; reference: { preview: string } }
  | {
      kind: "coverage";
      sources: {
        scope: Scope;
        label: string;
        coverage: Coverage;
        unavailable?: boolean;
      }[];
      next?: unknown;
      scope: QueryScope;
    }
  | { kind: "reset"; next?: unknown };
export interface HistoryPickerState {
  id: string;
  restoreFocus(): void;
  close(): void;
  busy: boolean;
  status: string;
  body?: Body;
  workspaces: { id: string; path: string }[];
  sources: { id: string; title: string; kind: string }[];
  fields: {
    range: string;
    workspace: string;
    kind: string;
    source: string;
    query: string;
  };
  set(field: string, value: string): void;
  choose(index: number): void;
  discover(reset?: boolean): Promise<void>;
  search(after?: unknown, scope?: QueryScope, query?: string): Promise<void>;
  read(
    scope: Scope,
    hit: Hit,
    offset?: number,
    allowed?: boolean,
  ): Promise<void>;
  freeze(start: number, end: number): Promise<void> | undefined;
  add(): void;
  inspect(after?: unknown, scope?: QueryScope): Promise<void>;
  rebuild(scope: Scope): Promise<void>;
  reset(after?: unknown): Promise<void>;
  coverage(value: Coverage): string;
}
export function HistoryPicker({ state }: { state: HistoryPickerState }) {
  const dialog = useOwnedDialog(state, 'input[type="search"]'),
    original = useRef<HTMLTextAreaElement>(null),
    body = state.body;
  const action = (text: string, run: () => unknown, primary = false) => (
    <button
      type="button"
      className={primary ? "primary" : "quiet"}
      disabled={state.busy}
      onClick={() => void run()}
    >
      {text}
    </button>
  );
  const select = (
    field: keyof HistoryPickerState["fields"],
    label: string,
    options: { id: string; title: string }[],
  ) => (
    <select
      aria-label={label}
      disabled={state.busy}
      value={state.fields[field]}
      onChange={(event) => state.set(field, event.target.value)}
    >
      {options.map((option) => (
        <option key={option.id} value={option.id}>
          {option.title}
        </option>
      ))}
    </select>
  );
  return (
    <dialog
      ref={dialog}
      className={`detail-dialog history-dialog ${css.scope}`}
      aria-label="Search conversation text"
      onClose={() => {
        if (!dialog.current?.open) state.close();
      }}
    >
      <div className="dialog-heading">
        <h2>Search conversation text</h2>
        <button type="button" className="quiet" onClick={() => state.close()}>
          Close
        </button>
      </div>
      <p className="hint">
        Search saved human, assistant and tool text across authorized
        workspaces. Results cover the indexed portion; continue indexing to
        expand coverage. Open the original before selecting a fragment.
      </p>
      {select("range", "History search range", [
        { id: "accessible_host", title: "All accessible workspaces" },
        { id: "workspace", title: "Selected workspace" },
        { id: "conversation", title: "Selected conversation" },
      ])}
      {select(
        "workspace",
        "History workspace",
        state.workspaces.map((item) => ({ id: item.id, title: item.path })),
      )}
      {select("kind", "History source kind", [
        { id: "native", title: "Native Session" },
        { id: "external", title: "External observations" },
      ])}
      <input
        aria-label="History conversation ID"
        maxLength={256}
        disabled={state.busy}
        value={state.fields.source}
        onChange={(event) => state.set("source", event.target.value)}
      />
      <select
        aria-label="Saved history source"
        disabled={state.busy}
        value={state.sources.findIndex(
          (source) =>
            source.id === state.fields.source &&
            source.kind === state.fields.kind,
        )}
        onChange={(event) => state.choose(Number(event.target.value))}
      >
        <option value={-1}>Choose a saved source…</option>
        {state.sources.map((source, index) => (
          <option key={`${source.kind}:${source.id}`} value={index}>
            {source.title}
          </option>
        ))}
      </select>
      <input
        type="search"
        aria-label="History text query"
        maxLength={256}
        disabled={state.busy}
        value={state.fields.query}
        onChange={(event) => state.set("query", event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            void state.search();
          }
        }}
      />
      <div className="actions">
        {action("Reset selected workspace index", () => state.reset())}
        {action("Continue indexing", () => state.discover())}
        {action("Search text", () => state.search(), true)}
        {action("Inspect coverage", () => state.inspect())}
        {action("Refresh discovery", () => state.discover(true))}
      </div>
      <p className="hint" role="status">
        {state.status}
      </p>
      <section className="history-results">
        {body?.kind === "matches" && (
          <>
            {body.matches.map((result, index) => (
              <article className="inline-card" key={index}>
                <p className="hint">
                  {result.label} · {result.scope.conversation.id} ·{" "}
                  {result.hit.original.record.kind} · record{" "}
                  {result.hit.original.record.sequence}
                </p>
                <pre>{result.hit.preview}</pre>
                {action("Open original", () =>
                  state.read(
                    result.scope,
                    result.hit,
                    0,
                    result.reference_allowed,
                  ),
                )}
              </article>
            ))}
            {!body.matches.length && <p>No matches in the indexed portion.</p>}
            {body.next != null &&
              action("More matches", () =>
                state.search(body.next, body.scope, body.query),
              )}
          </>
        )}
        {body?.kind === "original" && (
          <>
            <textarea
              ref={original}
              readOnly
              value={body.text}
              rows={12}
              aria-label="Verified original text"
            />
            <p className="hint">
              Select text in the original, then freeze the fragment.
            </p>
            {body.referenceAllowed ? (
              action(
                "Freeze selected fragment",
                () =>
                  state.freeze(
                    original.current!.selectionStart,
                    original.current!.selectionEnd,
                  ),
                true,
              )
            ) : (
              <p className="hint">
                Protected history can be read but cannot be frozen.
              </p>
            )}
            {body.offset > 0 &&
              action("Previous original page", () =>
                state.read(
                  body.scope,
                  body.hit,
                  Math.max(0, body.offset - 65536),
                  body.referenceAllowed,
                ),
              )}
            {body.has_more &&
              action("Next original page", () =>
                state.read(
                  body.scope,
                  body.hit,
                  body.next_offset,
                  body.referenceAllowed,
                ),
              )}
          </>
        )}
        {body?.kind === "frozen" && (
          <>
            <pre>{body.reference.preview}</pre>
            {action("Add selected reference to draft", () => state.add(), true)}
          </>
        )}
        {body?.kind === "coverage" && (
          <>
            {body.sources.map((source) => (
              <article
                className="inline-card"
                key={`${source.scope.workspace}:${source.scope.conversation.kind}:${source.scope.conversation.id}`}
              >
                <p>
                  {source.label} · {source.scope.conversation.id}
                </p>
                <p className="hint">
                  {state.coverage(source.coverage)}
                  {source.unavailable
                    ? " Unavailable in this pass; refresh to retry."
                    : ""}
                </p>
                {action("Rebuild this source", () =>
                  state.rebuild(source.scope),
                )}
              </article>
            ))}
            {body.next != null &&
              action("More source coverage", () =>
                state.inspect(body.next, body.scope),
              )}
          </>
        )}
        {body?.kind === "reset" &&
          body.next != null &&
          action("Continue workspace reset", () => state.reset(body.next))}
      </section>
    </dialog>
  );
}
