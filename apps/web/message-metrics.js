export function messageClock(timestamp, now = Date.now()) {
  const date = new Date(Number(timestamp));
  if (!timestamp || !Number.isFinite(date.getTime())) return null;
  const today = new Date(now);
  const sameDay = date.toDateString() === today.toDateString();
  return {
    iso: date.toISOString(),
    title: date.toLocaleString(),
    label: date.toLocaleString(undefined, { ...(sameDay ? {} : { month: 'short', day: 'numeric' }), hour: '2-digit', minute: '2-digit', hour12: false }),
  };
}
export function tokenCount(value) {
  if (!Number.isFinite(value) || value < 0) return '';
  if (value < 1000) return String(Math.round(value));
  if (value >= 1_000_000) return `${Number((value / 1_000_000).toFixed(1))}M`;
  return `${Number((value / 1000).toFixed(1))}K`;
}
export function tokenTotal(usage) {
  return usage ? usage.input_tokens + usage.output_tokens : undefined;
}
export function elapsedLabel(milliseconds) {
  if (!Number.isFinite(milliseconds) || milliseconds < 0) return '';
  const seconds = Math.round(milliseconds / 1000);
  return seconds < 60 ? `${seconds}s` : `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}
export function usageReadings(summary, capacity) {
  const usage = summary?.reported_attempts > 0 ? summary.tokens : undefined;
  const timing = summary?.timing;
  return {
    total: tokenTotal(usage),
    cache: usage?.input_tokens > 0 && usage.cache_read_tokens != null ? 100 * usage.cache_read_tokens / usage.input_tokens : undefined,
    speed: timing?.decode_ms > 0 ? 1000 * timing.decode_output_tokens / timing.decode_ms : undefined,
    context: summary?.last_context && capacity > 0 ? 100 * summary.last_context.input_tokens / capacity : undefined,
  };
}

export function durationReading(milliseconds) {
  if (!Number.isFinite(milliseconds) || milliseconds < 0) return '';
  return milliseconds < 60_000 ? `${Number((milliseconds / 1000).toFixed(1))}s` : elapsedLabel(milliseconds);
}
