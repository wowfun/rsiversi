// Only describe supplied evidence; absence of execution metadata is not Local authority.
export function approvalFields(review) {
  if (!review) return [];
  const location = review.execution?.binding?.location;
  const fields = [
    ["Execution location", location?.kind === "local" ? "Service machine" : location?.kind === "ssh" ? `SSH · ${location.target}` : location ? "Unrecognized execution location" : "Not supplied"],
    ["Working directory", review.cwd],
  ];
  if (review.execution?.workspace) fields.push(["Workspace", review.execution.workspace]);
  fields.push(["Requested sandbox policy", review.sandbox]);
  return fields;
}
const DISPLAY_BYTES = 256 * 1024;
const TRUNCATED = "\n[Display truncated]";
const encoder = new TextEncoder();
const decoder = new TextDecoder();
function displayWindow(text, budget) {
  const bytes = encoder.encode(text.slice(0, budget));
  if (text.length <= budget && bytes.length <= budget) return text;
  let end = budget - encoder.encode(TRUNCATED).length;
  while (end > 0 && (bytes[end] & 0xc0) === 0x80) end--;
  return decoder.decode(bytes.subarray(0, Math.max(0, end))) + TRUNCATED;
}
export function approvalJson(value) {
  return displayWindow(JSON.stringify(value, null, 2), DISPLAY_BYTES);
}
export function approvalArguments(value) {
  const entries = value !== null && typeof value === "object" && !Array.isArray(value)
    ? Object.entries(value) : [["Value", value]];
  const rows = []; let remaining = DISPLAY_BYTES;
  for (const [index, [key, item]] of entries.entries()) {
    const text = JSON.stringify(item, null, 2);
    const size = encoder.encode(key).length + encoder.encode(text).length;
    // Keep room to explain omission of later fields as well as a long value.
    if (size > remaining - (index + 1 < entries.length ? encoder.encode(TRUNCATED).length : 0)) {
      const label = remaining > 128 ? displayWindow(key, Math.min(1024, Math.floor(remaining / 4))) : "";
      rows.push([label, displayWindow(`${label ? "" : key + "\n"}${text}${TRUNCATED}`, remaining - encoder.encode(label).length)]);
      break;
    }
    rows.push([key, text]); remaining -= size;
  }
  return rows;
}

export function approvalReason(reason) {
  return /^Agent turn [^\s]+ requested this Tool effect$/.test(reason)
    ? "The assistant requested this tool action."
    : reason;
}

// Hold one human decision until it settles. Only a known refusal permits retry.
export function decisionAction(send, disable, report = () => {}, pending = () => {}) {
  let busy = false;
  return async value => {
    if (busy) return;
    busy = true; disable(true); report(""); pending(true);
    try { return await send(value); }
    catch (error) {
      report(`${error.message ?? String(error)}${error.notAdmitted ? "" : " The decision outcome is unknown. Refresh request checks current pending state before you decide again."}`);
      if (error.notAdmitted && error.retryable !== false) { busy = false; disable(false); }
    } finally { pending(false); }
  };
}
