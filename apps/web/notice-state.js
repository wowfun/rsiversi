// Independent notice lifetimes share a snapshot, never a concatenated DOM write.
export function updateNotice(current, kind, message) {
  return current[kind] === message ? current : {...current, [kind]: message};
}
