// This marker selects conveniences only. Native URL/window ACLs own authority.
export function isDesktop(scope = globalThis) {
  return scope.__RSI_DESKTOP__ === true && typeof scope.__TAURI_INTERNALS__?.invoke === "function";
}
export function desktopCall(operation, scope = globalThis, descriptor = typeof __RSI_DESKTOP_BRIDGE__ === "object" ? __RSI_DESKTOP_BRIDGE__ : undefined) {
  if (!isDesktop(scope) || !descriptor) return Promise.reject(new Error("Desktop bridge is unavailable"));
  return scope.__TAURI_INTERNALS__.invoke("rsi_bridge", {payload:JSON.stringify({protocol:descriptor,operation})});
}

export async function saveDesktopExport(call, pane, generation, args, signal) {
  const invoke = operation => call("export_input", JSON.stringify({pane,generation,operation})).then(JSON.parse);
  const opened = await invoke({kind:"open",arguments:args});
  let native, committed = false;
  const abort = () => {
    void invoke({kind:"cancel",token:opened.token}).catch(()=>{});
    if (native) void desktopCall({kind:"export_cancel",token:native.token}).catch(()=>{});
  };
  signal.addEventListener("abort",abort,{once:true});
  try {
    if (signal.aborted) throw new Error("Export cancelled");
    native = await desktopCall({kind:"export_open",event:opened.event});
    for (;;) {
      if (signal.aborted) throw new Error("Export cancelled");
      const event = await invoke({kind:"next",token:opened.token});
      if (!event) throw new Error("Export ended without completion");
      await desktopCall({kind:"export_write",token:native.token,event});
      if (event.type === "complete") {
        if (await invoke({kind:"next",token:opened.token}) !== null) throw new Error("Export continued after completion");
        const result = await desktopCall({kind:"export_finish",token:native.token});
        committed = true;
        return result;
      }
    }
  } finally {
    signal.removeEventListener("abort",abort);
    await invoke({kind:"cancel",token:opened.token}).catch(()=>{});
    if (native && !committed) await desktopCall({kind:"export_cancel",token:native.token}).catch(()=>{});
  }
}
