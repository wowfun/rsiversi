import type {SettingsField, SettingsFields} from '../runtime/settings-fields.js';
import type {SettingsDraft} from '../runtime/settings-draft.js';
import type {ChangeEvent, ReactNode} from 'react';
import css from "./settings.module.css";
import { settingsFocus } from "../../settings-focus.js";
import {
  Component,
  createRef,
  useLayoutEffect,
  useRef,
  useSyncExternalStore,
} from "react";
import { registeredSettings, run } from "../bridge.ts";
function SchemaField({ field, editor }: {field: SettingsField; editor: SettingsFields}) {
  const { schema, label, title } = field,
    change = (event: ChangeEvent<HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement>) =>
      editor.edit(
        field,
        field.kind === "boolean" ? (event.target as HTMLInputElement).checked : event.target.value,
      );
  let content;
  if (field.kind === "object")
    content = (
      <fieldset className="settings-object">
        <legend>{title}</legend>
        <div className="settings-fields">
          {field.parts.map((part) => (
            <div key={part.key}>
              {!part.required && (
                <label className="optional">
                  <input
                    type="checkbox"
                    aria-label={`Include ${label} / ${part.key}`}
                    checked={part.included}
                    onChange={(event) =>
                      editor.include(part, event.target.checked)
                    }
                  />
                  Include {part.key.replaceAll("_", " ")}
                </label>
              )}
              <fieldset
                className="settings-field-controls"
                disabled={!part.required && !part.included}
                hidden={!part.required && !part.included}
              >
                <SchemaField field={part.field} editor={editor} />
              </fieldset>
            </div>
          ))}
        </div>
      </fieldset>
    );
  else if (field.kind === "enum")
    content = (
      <label>
        {title}
        <select aria-label={label} value={field.value} onChange={change}>
          {field.schema.enum.map((value, index) => (
            <option key={index} value={index}>
              {typeof value === "string" ? value : JSON.stringify(value)}
            </option>
          ))}
        </select>
      </label>
    );
  else if (field.kind === "json")
    content = (
      <label>
        {title} (JSON)
        <textarea
          className="settings-text"
          value={field.value}
          spellCheck={false}
          aria-label={`${label} JSON`}
          onChange={change}
        />
      </label>
    );
  else if (field.kind === "boolean")
    content = (
      <label>
        {title}
        <input
          type="checkbox"
          checked={field.value}
          aria-label={label}
          onChange={change}
        />
      </label>
    );
  else if (field.kind === "string")
    content = (
      <label>
        {title}
        {field.value.includes("\n") ? (
          <textarea aria-label={label} value={field.value} onChange={change} />
        ) : (
          <input
            type="text"
            aria-label={label}
            value={field.value}
            minLength={
              typeof schema.minLength === "number" && Number.isSafeInteger(schema.minLength) && schema.minLength >= 0
                ? schema.minLength
                : undefined
            }
            maxLength={
              typeof schema.maxLength === "number" && Number.isSafeInteger(schema.maxLength) && schema.maxLength >= 0
                ? schema.maxLength
                : undefined
            }
            onChange={change}
          />
        )}
      </label>
    );
  else
    content = (
      <label>
        {title}
        <input
          type="number"
          aria-label={label}
          value={field.value}
          required
          step={schema.multipleOf ?? (field.kind === "integer" ? 1 : "any")}
          min={schema.minimum}
          max={schema.maximum}
          onChange={change}
        />
      </label>
    );
  return (
    <div className="settings-field">
      {content}
      {field.description && <p className="hint">{field.description}</p>}
    </div>
  );
}
class SettingsFocus extends Component<{children: ReactNode}> {
  root = createRef<HTMLDivElement>();
  getSnapshotBeforeUpdate() {
    return settingsFocus(this.root.current);
  }
  componentDidUpdate(_props: unknown, _state: unknown, restore: () => void) {
    restore();
  }
  render() {
    return (
      <div ref={this.root} className={`registered-editor ${css.scope}`}>
        {this.props.children}
      </div>
    );
  }
}
export function RegisteredDraft({ draft }: {draft: SettingsDraft}) {
  useSyncExternalStore(draft.subscribe, draft.getSnapshot);
  const form = useRef<HTMLFormElement>(null),
    editor = draft.fields,
    previous = useRef<boolean | undefined>(undefined),
    description = draft.current.description,
    metadata = description.metadata;
  // A focused action becoming disabled must leave an accessible recovery target.
  useLayoutEffect(() => {
    const root = form.current,
      active = document.activeElement;
    if (active instanceof HTMLElement && root?.contains(active) && active.matches(":disabled"))
      root
        ?.querySelector<HTMLElement>(
          "button:not(:disabled),input:not(:disabled),textarea:not(:disabled),select:not(:disabled)",
        )
        ?.focus();
    if (previous.current !== undefined && previous.current !== editor.jsonMode)
      root
        ?.querySelector<HTMLElement>(
          editor.jsonMode
            ? 'textarea[aria-label="Settings JSON"]'
            : ".settings-object input,.settings-object select,.settings-object textarea",
        )
        ?.focus();
    previous.current = editor.jsonMode;
  });
  return (
    <SettingsFocus>
      <form
        ref={form}
        data-settings-ticket={draft.current.ticket}
        data-settings-namespace={draft.current.namespace}
        onSubmit={(event) => {
          event.preventDefault();
          void draft.save();
        }}
      >
        <p className="settings-applies">
          {
            {
              live: "Applies live",
              new_session: "Applies to new conversations",
              restart: "Restart required",
            }[metadata.applies]
          }
        </p>
        <p className="hint">{metadata.description}</p>
        {([
          ["Schema", metadata.schema],
          ["Defaults", description.defaults],
        ] as const).map(([label, value]) => (
          <details key={label} className="settings-description">
            <summary>{label}</summary>
            <pre>{JSON.stringify(value, null, 2)}</pre>
          </details>
        ))}
        {!!metadata.sensitive_fields.length && (
          <p className="hint">
            Sensitive fields:{" "}
            {metadata.sensitive_fields
              .map((path) => path.join(" / ") || "(root)")
              .join(", ")}
          </p>
        )}
        <fieldset disabled={draft.disabled}>
          <div className="settings-editor">
            <fieldset
              className="settings-object"
              hidden={editor.jsonMode}
              disabled={editor.locked}
            >
              {editor.fields && (
                <SchemaField field={editor.fields} editor={editor} />
              )}
            </fieldset>
            <button
              type="button"
              disabled={editor.toggleDisabled}
              onClick={() => editor.toggle()}
            >
              {editor.jsonMode ? "Use form" : "Edit as JSON"}
            </button>
            <textarea
              className="settings-text"
              value={editor.text}
              readOnly={editor.locked}
              spellCheck={false}
              aria-label="Settings JSON"
              hidden={!editor.jsonMode}
              onChange={(event) => editor.editJson(event.target.value)}
            />
            <p className="hint" role="status" hidden={!editor.reason}>
              {editor.reason}
            </p>
          </div>
        </fieldset>
        <p className="settings-error settings-field-error" role="alert">
          {draft.error}
        </p>
        <button type="submit" className="primary" disabled={!draft.canSave}>
          Save settings
        </button>
        <button
          type="button"
          hidden={!draft.reloadVisible}
          disabled={draft.saving}
          onClick={() => void draft.reload()}
        >
          {draft.awaiting ? "Reload settings" : "Discard edits and reload"}
        </button>
        {!description.writable && (
          <p className="hint">Settings provider is read-only.</p>
        )}
        {draft.current.namespace !== "rsi.client" && (
          <button type="button" onClick={() => void draft.browse()}>
            Browse registered settings
          </button>
        )}
      </form>
    </SettingsFocus>
  );
}
export function RegisteredSettings() {
  const state = useSyncExternalStore(
    registeredSettings.subscribe,
    registeredSettings.getSnapshot,
  );
  if (state && "draft" in state) return <RegisteredDraft draft={state.draft} />;
  if (!state) return <p>Loading settings…</p>;
  const { catalog, command, revision } = state;
  const reload = () =>
    command(
      catalog.namespace
        ? { action: "settings_read", namespace: catalog.namespace }
        : { action: "settings_list" },
    );
  return (
    <div className={`settings-catalog ${css.scope}`}>
      {catalog.error ? (
        <>
          <p className="settings-error">{catalog.error}</p>
          <button type="button" onClick={() => void run(reload)}>
            Reload settings
          </button>
        </>
      ) : !catalog.page ? (
        <p>
          {catalog.namespace
            ? `Reading ${catalog.namespace}…`
            : "Loading registered settings…"}
        </p>
      ) : (
        <>
          <div className="settings-list">
            {catalog.page.namespaces.map((namespace) => (
              <button
                type="button"
                key={namespace}
                className="settings-namespace quiet"
                onClick={() =>
                  void run(() =>
                    command({
                      action: "settings_read",
                      namespace,
                      window: revision,
                    }),
                  )
                }
              >
                {namespace}
              </button>
            ))}
            {!catalog.page.namespaces.length && <p>No registered settings.</p>}
          </div>
          <div className="actions">
            <button
              type="button"
              onClick={() =>
                void run(() => command({ action: "settings_list" }))
              }
            >
              Refresh settings
            </button>
            <button
              type="button"
              disabled={catalog.page.next == null}
              onClick={() =>
                void run(() =>
                  command({ action: "settings_next", ticket: catalog.ticket }),
                )
              }
            >
              More settings
            </button>
          </div>
        </>
      )}
    </div>
  );
}
