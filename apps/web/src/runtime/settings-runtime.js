import { SettingsDialog } from "../../settings-dialog.js";
import { createSettingsDraft } from "./settings-draft.js";
import {
  settingsPanel,
  settingsMounted,
  advancedSettings,
  registeredSettings,
} from "../bridge.ts";

export function createSettingsRuntime(context) {
  const drafts = new Map(),
    subscriptions = [];
  let selecting = false,
    read;
  const clear = () => {
    for (const draft of drafts.values()) draft.retire();
    drafts.clear();
    registeredSettings.set(undefined);
    advancedSettings.set(undefined);
  };
  const dialog = new SettingsDialog({
    close() {
      read = undefined;
      settingsPanel.set(undefined);
    },
    send(owner, revision) {
      return context.boundCall(owner)(
        "command",
        JSON.stringify({ action: "close_settings", revision }),
      );
    },
    failed(owner, error) {
      if (context.connection === owner)
        context.notify(String(error.message ?? error));
    },
  });
  function command(value) {
    const reading =
        value.action === "settings_read" || value.action === "settings_list",
      owner = context.connection,
      request = reading ? {} : undefined;
    if (reading) {
      read = request;
      dialog.begin(owner, context.view?.settings_revision);
      if (value.namespace !== "rsi.client")
        advancedSettings.set(value.namespace);
      selecting = true;
      try {
        settingsPanel.set(
          value.namespace === "rsi.client" ? "appearance" : "advanced",
        );
      } finally {
        selecting = false;
      }
      registeredSettings.set(undefined);
    }
    return context
      .boundCall(owner)("command", JSON.stringify(value))
      .catch((error) => {
        if (
          reading &&
          read === request &&
          context.connection === owner &&
          settingsPanel.getSnapshot() ===
            (value.namespace === "rsi.client" ? "appearance" : "advanced")
        )
          registeredSettings.set({
            catalog: {
              namespace: value.namespace,
              error: String(error.message ?? error),
            },
            command,
          });
        throw error;
      });
  }
  function present(next) {
    if (
      !dialog.update(context.connection, next.settings_revision) ||
      !settingsMounted.getSnapshot()
    )
      return;
    const editor = next.settings,
      catalog = next.settings_catalog;
    if (editor) {
      const section =
        editor.namespace === "rsi.client" ? "appearance" : "advanced";
      if (settingsPanel.getSnapshot() !== section) return;
      if (section === "advanced") advancedSettings.set(editor.namespace);
      let draft = drafts.get(section);
      if (!draft || draft.current.namespace !== editor.namespace) {
        draft?.retire();
        draft = createSettingsDraft(editor, command);
        drafts.set(section, draft);
      } else if (draft.current !== editor) draft.update(editor);
      if (registeredSettings.getSnapshot()?.draft !== draft)
        registeredSettings.set({ draft });
    } else if (catalog) {
      if (
        settingsPanel.getSnapshot() !==
        (catalog.namespace === "rsi.client" ? "appearance" : "advanced")
      )
        return;
      if (!catalog.namespace) advancedSettings.set(undefined);
      registeredSettings.set({
        catalog,
        revision: next.settings_revision,
        command,
      });
    } else registeredSettings.set(undefined);
  }
  subscriptions.push(
    settingsPanel.subscribe(() => {
      if (selecting) return;
      const section = settingsPanel.getSnapshot(),
        namespace = advancedSettings.getSnapshot(),
        request =
          section === "appearance"
            ? { action: "settings_read", namespace: "rsi.client" }
            : section === "advanced"
              ? namespace
                ? { action: "settings_read", namespace }
                : { action: "settings_list" }
              : undefined;
      if (request)
        void command(request).catch((error) =>
          context.notify(String(error.message ?? error)),
        );
    }),
  );
  subscriptions.push(
    settingsMounted.subscribe(() => {
      if (!settingsMounted.getSnapshot()) clear();
      else if (context.view) present(context.view);
    }),
  );
  return {
    command,
    present,
    dismiss: () => dialog.dismiss(),
    clear() {
      dialog.reset();
      clear();
    },
    dispose() {
      for (const off of subscriptions) off();
      clear();
    },
  };
}
