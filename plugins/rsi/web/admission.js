// Shared document/Worker transport bounds; Rust validates the actual request.
export const limits = Object.freeze({ ordinary: 8, terminalRead: 32, terminalWrite: 8, lifecycle: 1, exportControl: 8 });
export function lane(method, payload) {
  if (method === "export_cancel") return "exportControl";
  if (method === "connect" || method === "disconnect") return "lifecycle";
  if (method === "terminal" && typeof payload === "string" && payload.length <= 512 * 1024) {
    try {
      const type = JSON.parse(payload)?.request?.type;
      if (type === "read") return "terminalRead";
      if (type === "write") return "terminalWrite";
    } catch { /* Malformed inputs use ordinary admission before Rust rejects them. */ }
  }
  return "ordinary";
}
