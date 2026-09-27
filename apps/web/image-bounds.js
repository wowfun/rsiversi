// Inspect encoded structure before handing any workspace image to a browser decoder.
export const maximumImagePixels = 16_777_216;
export const maximumPreviewPixels = 2 * maximumImagePixels;
export function imageBudget() {
  let remaining = maximumPreviewPixels;
  return pixels => {
    if (pixels > remaining) throw new Error('Preview exceeds its aggregate pixel limit');
    remaining -= pixels;
  };
}
// Blob consumers may sniff raster bytes independently of the supplied MIME label.
export function boundedResource(data, mime, admitPixels) {
  const prefix = String.fromCharCode(...data.subarray(0, 12));
  const raster = prefix.startsWith('\x89PNG\r\n\x1a\n') || prefix.startsWith('GIF8') ||
    prefix.startsWith('\xff\xd8') || prefix.startsWith('BM') ||
    (prefix.startsWith('RIFF') && prefix.slice(8) === 'WEBP') || prefix.startsWith('\0\0\x01\0');
  if (mime.startsWith('image/') || raster) return boundedImage(data, mime, admitPixels);
  // Non-image resources are text or the supported font containers. Unknown binary
  // formats must not become browser-sniffable image blobs through a false MIME label.
  if (['wOFF','wOF2','OTTO','true','ttcf','\0\x01\0\0'].includes(prefix.slice(0,4))) return data;
  const text = new TextDecoder('utf-8', {fatal:true}).decode(data);
  if (/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/.test(text)) throw new Error('Unsupported binary preview resource');
  return data;
}
export function boundedImage(data, mime, admitPixels = () => {}) {
  const bad = () => { throw new Error('Image has unsupported or malformed headers'); };
  const animated = () => { throw new Error('Animated image preview is unavailable; use Source or hex'); };
  let pixels = 0;
  const dimensions = (width, height) => {
    if (!Number.isSafeInteger(width) || !Number.isSafeInteger(height) || width <= 0 || height <= 0) bad();
    if (width * height > maximumImagePixels) throw new Error('Image exceeds the 16,777,216 pixel limit');
    pixels = Math.max(pixels, width * height);
  };
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  const span = (offset, length) => { if (offset < 0 || length < 0 || offset + length > data.length) bad(); };
  const u16 = (offset, little = false) => { span(offset, 2); return view.getUint16(offset, little); };
  const u32 = (offset, little = false) => { span(offset, 4); return view.getUint32(offset, little); };
  const tag = (offset, length) => { span(offset, length); return String.fromCharCode(...data.subarray(offset, offset + length)); };
  if (mime === 'image/svg+xml') {
    if (data.byteLength > 256 * 1024) throw new Error('SVG exceeds its 256 KiB source limit');
    const parsed = new DOMParser().parseFromString(new TextDecoder('utf-8', {fatal:true}).decode(data), 'image/svg+xml');
    const root = parsed.documentElement;
    if (root.localName !== 'svg' || root.namespaceURI !== 'http://www.w3.org/2000/svg' || parsed.querySelector('parsererror')) bad();
    // Embedded raster payloads and foreign DOM would bypass the raster header gate.
    if (parsed.querySelector('image, feImage, foreignObject, use, animate, animateMotion, animateTransform, set, style') || /(?:data|blob):/i.test(root.outerHTML)) bad();
    const stack = [[root, 1]]; let nodes = 0;
    while (stack.length) {
      const [node, depth] = stack.pop();
      if (++nodes > 2048 || depth > 32) throw new Error('SVG exceeds its node or nesting limit');
      for (const child of node.childNodes) stack.push([child, depth + 1]);
    }
    const box = (root.getAttribute('viewBox') ?? '').trim().split(/[\s,]+/).map(Number);
    const units = { '':1, px:1, in:96, cm:96/2.54, mm:96/25.4, pt:96/72, pc:16, q:96/101.6 };
    const length = (attribute, fallback) => {
      const raw = root.style.getPropertyValue(attribute) || root.getAttribute(attribute);
      if (!raw) return fallback;
      const match = /^\s*(\d+(?:\.\d+)?)(px|in|cm|mm|pt|pc|q)?\s*$/i.exec(raw);
      if (!match) bad();
      return Math.ceil(Number(match[1]) * units[(match[2] ?? '').toLowerCase()]);
    };
    const width = length('width', box.length === 4 ? Math.ceil(box[2]) : 300);
    const height = length('height', box.length === 4 ? Math.ceil(box[3]) : 150);
    dimensions(width, height);
    root.setAttribute('width', String(width)); root.setAttribute('height', String(height));
    root.style.setProperty('width', `${width}px`, 'important'); root.style.setProperty('height', `${height}px`, 'important');
    admitPixels(pixels);
    return new TextEncoder().encode(new XMLSerializer().serializeToString(root));
  }
  if (data.length < 10) bad();
  if (tag(0, 8) === '\x89PNG\r\n\x1a\n') {
    if (u32(8) !== 13 || tag(12, 4) !== 'IHDR') bad();
    dimensions(u32(16), u32(20));
    let ended = false;
    for (let offset = 8; offset < data.length;) {
      const size = u32(offset), kind = tag(offset + 4, 4); span(offset, size + 12);
      if (kind === 'acTL') animated();
      if (kind === 'IHDR' && offset !== 8) bad();
      offset += size + 12;
      if (kind === 'IEND') { if (offset !== data.length || size !== 0) bad(); ended = true; }
    }
    if (!ended) bad();
  } else if (tag(0, 3) === 'GIF') {
    if (!['GIF87a', 'GIF89a'].includes(tag(0, 6))) bad();
    const width = u16(6, true), height = u16(8, true); dimensions(width, height); span(0, 13);
    let offset = 13 + (data[10] & 128 ? 3 * (2 << (data[10] & 7)) : 0), frames = 0, ended = false;
    const blocks = () => { for (;;) { span(offset, 1); const size = data[offset++]; if (!size) break; span(offset, size); offset += size; } };
    while (offset < data.length) {
      const kind = data[offset++];
      if (kind === 0x3b) { ended = true; break; }
      if (kind === 0x21) { span(offset++, 1); blocks(); }
      else if (kind === 0x2c) {
        span(offset, 9); dimensions(u16(offset + 4, true), u16(offset + 6, true));
        if (u16(offset, true) + u16(offset + 4, true) > width || u16(offset + 2, true) + u16(offset + 6, true) > height) bad();
        if (++frames > 1) animated();
        const packed = data[offset + 8]; offset += 9 + (packed & 128 ? 3 * (2 << (packed & 7)) : 0);
        span(offset++, 1); blocks();
      } else bad();
    }
    if (!ended || !frames || offset !== data.length) bad();
  } else if (tag(0, 4) === 'RIFF' && tag(8, 4) === 'WEBP') {
    if (u32(4, true) + 8 !== data.length) bad();
    let found = false;
    for (let offset = 12; offset < data.length;) {
      const kind = tag(offset, 4), size = u32(offset + 4, true), start = offset + 8; span(start, size);
      if (kind === 'ANIM' || kind === 'ANMF') animated();
      if (kind === 'VP8X') {
        if (size !== 10) bad(); if (data[start] & 2) animated();
        const n24 = p => data[p] + (data[p + 1] << 8) + (data[p + 2] << 16) + 1;
        dimensions(n24(start + 4), n24(start + 7));
      } else if (kind === 'VP8 ') {
        if (size < 10 || tag(start + 3, 3) !== '\x9d\x01\x2a') bad();
        dimensions(u16(start + 6, true) & 0x3fff, u16(start + 8, true) & 0x3fff); found = true;
      } else if (kind === 'VP8L') {
        if (size < 5 || data[start] !== 0x2f) bad(); const bits = u32(start + 1, true);
        dimensions((bits & 0x3fff) + 1, ((bits >>> 14) & 0x3fff) + 1); found = true;
      }
      offset = start + size + (size & 1); span(offset, 0);
    }
    if (!found) bad();
  } else if (u16(0) === 0xffd8) {
    let found = false;
    for (let offset = 2; offset < data.length;) {
      if (data[offset++] !== 0xff) bad();
      while (data[offset] === 0xff) offset++;
      span(offset, 1); const marker = data[offset++];
      if (marker === 0xda || marker === 0xd9) break;
      if (marker === 0x01 || (marker >= 0xd0 && marker <= 0xd7)) continue;
      const size = u16(offset); if (size < 2) bad(); span(offset, size);
      if ([0xc0,0xc1,0xc2,0xc3,0xc5,0xc6,0xc7,0xc9,0xca,0xcb,0xcd,0xce,0xcf].includes(marker)) {
        if (size < 8 || found) bad(); dimensions(u16(offset + 5), u16(offset + 3)); found = true;
      }
      offset += size;
    }
    if (!found) bad();
  } else if (tag(0, 2) === 'BM') {
    const dib = u32(14, true);
    if (dib === 12) dimensions(u16(18, true), u16(20, true));
    else if ([40,52,56,108,124].includes(dib)) { span(14, dib); dimensions(view.getInt32(18, true), Math.abs(view.getInt32(22, true))); }
    else bad();
  } else if (u16(0, true) === 0 && u16(2, true) === 1) {
    const count = u16(4, true); if (!count || count > 256) bad(); span(6, count * 16);
    for (let index = 0; index < count; index++) {
      const offset = 6 + index * 16, start = u32(offset + 12, true), size = u32(offset + 8, true);
      span(start, size);
      if (size >= 8 && tag(start, 8) === '\x89PNG\r\n\x1a\n') boundedImage(data.subarray(start, start + size), 'image/png', size => { pixels = Math.max(pixels, size); });
      else { if (size < 40 || u32(start, true) !== 40) bad(); dimensions(u32(start + 4, true), u32(start + 8, true)); }
    }
  } else bad();
  admitPixels(pixels);
  return data;
}
