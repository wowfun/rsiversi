export function previewDocument(bootstrap) {
  // The HTML tokenizer recognizes script end tags even inside JS strings/comments.
  const script = bootstrap.replace(/<\/script/gi, match => `<\\/${match.slice(2)}`);
  return `<!doctype html><html><head><meta charset="utf-8"><title>File preview</title></head><body><script>${script}</script></body></html>`;
}
