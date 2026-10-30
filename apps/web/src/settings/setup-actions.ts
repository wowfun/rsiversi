import { useRef, useState } from "react";
import { input, run } from "../bridge.ts";

export function useSetupActions() {
  const [busy, setBusy] = useState(false),
    pending = useRef(false);
  const command = (command: Record<string, unknown>) =>
    input.command({ action: "setup", command });
  const act = (work: () => Promise<unknown>) => {
    if (pending.current) return;
    pending.current = true;
    setBusy(true);
    void run(async () => {
      try {
        await work();
      } finally {
        pending.current = false;
        setBusy(false);
      }
    });
  };
  return { busy, command, act };
}
export type SetupActions = ReturnType<typeof useSetupActions>;
