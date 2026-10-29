import { run } from "../bridge.ts";
import css from "./surface.module.css";
import { useState } from "react";
import { useOwnedDialog } from "./use-owned-dialog.ts";

export interface FilePickerState {
  id: string;
  page?: {
    kind: "file" | "directory";
    path: string;
    label: string;
    parent?: string;
    revision: string;
    offset: string;
    next_offset: string;
    total: string;
    more: boolean;
    text?: string;
    entries?: {
      path: string;
      kind: "file" | "directory" | null;
      name: string;
    }[];
  };
  loading: boolean;
  status: string;
  filter: string;
  open(path: string, kind: string): Promise<void>;
  openText(path: string, kind: string): Promise<void> | undefined;
  read(operation: Record<string, unknown>): Promise<void>;
  setFilter(value: string): void;
  insert(): void;
  reference(): void;
  close(): void;
  restoreFocus(): void;
}
export function FilePicker({ state }: { state: FilePickerState }) {
  const dialog = useOwnedDialog(state, '[aria-label="Filter visible files"]'),
    [path, setPath] = useState("");
  const page = state.page,
    entries =
      page?.entries?.filter((entry) =>
        entry.name
          .toLocaleLowerCase()
          .includes(state.filter.toLocaleLowerCase()),
      ) ?? [];
  const offset = page ? BigInt(page.offset) : 0n,
    step = page?.kind === "file" ? 4096n : 16n;
  const requestPage = (offset: string, hex: boolean) =>
    void state.read({ kind: "page", revision: page!.revision, offset, hex });
  return (
    <dialog
      ref={dialog}
      className={`detail-dialog file-picker-dialog ${css.scope}`}
      aria-label="Insert workspace file path"
      onClose={() => {
        if (!dialog.current?.open) state.close();
      }}
    >
      <div className="dialog-heading">
        <h2>Workspace file</h2>
        <button type="button" onClick={() => state.close()}>
          Close
        </button>
      </div>
      <p className="hint">
        Browse and preview a file, then insert its path. File contents enter the
        conversation only when a tool reads them.
      </p>
      <input
        placeholder="Workspace-relative path"
        maxLength={4096}
        aria-label="Workspace-relative file path"
        value={path}
        onChange={(event) => setPath(event.target.value)}
      />
      <div className="actions">
        <button
          type="button"
          disabled={state.loading}
          onClick={() => void state.openText(path, "directory")}
        >
          List directory
        </button>
        <button
          type="button"
          disabled={state.loading}
          onClick={() => void state.openText(path, "file")}
        >
          Preview file
        </button>
        <button
          type="button"
          disabled={state.loading}
          onClick={() => void state.open("", "directory")}
        >
          Workspace root
        </button>
        <button
          type="button"
          disabled={state.loading}
          onClick={() => void run(() => state.reference())}
        >
          Reference conversation
        </button>
      </div>
      <input
        type="search"
        placeholder="Filter this directory page"
        aria-label="Filter visible files"
        hidden={!!page && page.kind !== "directory"}
        disabled={state.loading}
        value={state.filter}
        onChange={(event) => state.setFilter(event.target.value)}
      />
      <p className="hint" role="status">
        {state.status}
      </p>
      <section className="file-picker-content">
        {page && (
          <>
            <p className="reference-origin">{page.label}</p>
            <p className="hint">
              {page.kind === "file" ? "Bytes" : "Entries"} {page.offset}–
              {page.next_offset} / {page.total}
            </p>
            {page.parent !== undefined && (
              <button
                type="button"
                disabled={state.loading}
                onClick={() => void state.open(page.parent!, "directory")}
              >
                Parent directory
              </button>
            )}
            {page.kind === "directory" ? (
              <div className="file-choice-list">
                {entries.map((entry) => (
                  <button
                    key={entry.path}
                    type="button"
                    disabled={!entry.kind || state.loading}
                    onClick={() => void state.open(entry.path, entry.kind!)}
                  >
                    {entry.name}
                    {entry.kind === "directory"
                      ? "/"
                      : entry.kind
                        ? " · preview"
                        : " · unavailable"}
                  </button>
                ))}
                {!entries.length && (
                  <p className="hint">No matching files on this page</p>
                )}
              </div>
            ) : (
              <>
                <pre>{page.text}</pre>
                <button
                  type="button"
                  className="primary"
                  disabled={state.loading}
                  onClick={() => state.insert()}
                >
                  Insert file path
                </button>
                <button
                  type="button"
                  disabled={state.loading}
                  onClick={() => requestPage(page.offset, true)}
                >
                  View exact hex
                </button>
                <button
                  type="button"
                  disabled={state.loading}
                  onClick={() => requestPage(page.offset, false)}
                >
                  View text
                </button>
              </>
            )}
            {offset > 0n && (
              <button
                type="button"
                disabled={state.loading}
                onClick={() =>
                  requestPage(String(offset > step ? offset - step : 0n), false)
                }
              >
                Previous page
              </button>
            )}
            {page.more && (
              <button
                type="button"
                disabled={state.loading}
                onClick={() => requestPage(page.next_offset, false)}
              >
                Next page
              </button>
            )}
            <button
              type="button"
              disabled={state.loading}
              onClick={() => void state.open(page.path, page.kind)}
            >
              Refresh current path
            </button>
          </>
        )}
      </section>
    </dialog>
  );
}
