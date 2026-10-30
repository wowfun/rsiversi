import { useState } from "react";
import { Button } from "../button.tsx";
import { useView } from "../bridge.ts";
import type { SetupActions } from "./setup-actions.ts";
export function ConversationDefaults({ actions }: { actions: SetupActions }) {
  const { busy, act, command } = actions,
    setup = useView((view) => view?.setup);
  const [preset, setPreset] = useState(""),
    locked = !setup?.allowed || busy;
  return (
    <section className="setup-section">
      <h2>Conversation defaults</h2>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          act(() =>
            command({ kind: "default_preset", ticket: setup?.ticket, preset }),
          );
        }}
      >
        <label>
          Default preset
          <input
            aria-label="Default preset"
            required
            placeholder={setup?.presets?.default ?? "Existing preset identity"}
            value={preset}
            onChange={(e) => setPreset(e.target.value)}
            disabled={locked || !setup?.settings_writable}
          />
        </label>
        <Button
          type="submit"
          size="sm"
          disabled={locked || !setup?.settings_writable}
        >
          Save preset selection
        </Button>
      </form>
    </section>
  );
}
