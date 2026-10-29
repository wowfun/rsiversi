// Schema-directed input assistance. The registered Rust validator owns acceptance.
// JSON.parse would round large Rust integer values. Preserve their original JSON editor.
function canUseSettingsForm(text) {
  for (const match of text.matchAll(
    /"(?:[^"\\]|\\.)*"|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/gs,
  )) {
    if (match[1] && JSON.stringify(Number(match[1])) !== match[1]) return false;
    if (
      match[1] &&
      /^-?\d+$/.test(match[1]) &&
      (BigInt(match[1]) > BigInt(Number.MAX_SAFE_INTEGER) ||
        BigInt(match[1]) < BigInt(Number.MIN_SAFE_INTEGER))
    )
      return false;
  }
  return true;
}

export const MAXIMUM_SETTINGS_BYTES = 1024 * 1024;
export const SETTINGS_LIMIT_MESSAGE = "This value exceeds the Web editor's 1 MiB input limit.";
const encoder = new TextEncoder();
export const settingsLimitExceeded = (text) =>
  text.length > MAXIMUM_SETTINGS_BYTES ||
  (text.length > MAXIMUM_SETTINGS_BYTES / 3 &&
    encoder.encode(text).length > MAXIMUM_SETTINGS_BYTES);
export function formUnavailable(text) {
  if (settingsLimitExceeded(text)) return SETTINGS_LIMIT_MESSAGE;
  try {
    JSON.parse(text);
  } catch {
    return "Enter valid JSON before switching to the form.";
  }
  return canUseSettingsForm(text)
    ? ""
    : "This value needs JSON to preserve its exact numbers.";
}
