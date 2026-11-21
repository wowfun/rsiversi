// Device layout only. Submission durability belongs to drafts.js and Rust.
export const defaultLayout = Object.freeze({version:6,conversationWidth:null,resourcesClosed:true,detail:'standard',workspaces:[]});
export function parseLayout(value) {
  const fallback = () => {throw new Error('Invalid layout');};
  if (!value || typeof value !== 'object' || Array.isArray(value)) return fallback();
  if (Object.keys(value).sort().join() !== Object.keys(defaultLayout).sort().join() || value.version!==6 ||
      (value.conversationWidth!==null && (!Number.isSafeInteger(value.conversationWidth) || value.conversationWidth<640)) ||
      typeof value.resourcesClosed!=='boolean' ||
      !['compact','standard','detailed','verbose'].includes(value.detail) || !Array.isArray(value.workspaces) || value.workspaces.length>16) return fallback();
  const ids=new Set();
  for(const item of value.workspaces) {
    if (!item || Object.keys(item).sort().join()!=='expanded,id' || typeof item.id!=='string' || !/^[a-zA-Z0-9_-]{1,128}$/.test(item.id) || typeof item.expanded!=='boolean' || ids.has(item.id)) return fallback();
    ids.add(item.id);
  }
  return {...value,workspaces:value.workspaces.map(item=>({...item}))};
}
export function workspacePreference(layout,id,expanded) {
  return validateLayout({...layout,workspaces:[...layout.workspaces.filter(item=>item.id!==id),{id,expanded}].slice(-16)});
}

export function validateLayout(value) {
  try {return parseLayout(value);} catch {return structuredClone(defaultLayout);}
}

export function conversationWidth(preferred,column) {
  if(!Number.isFinite(column) || column<0)throw new Error('Invalid conversation column');
  if(preferred!==null && (!Number.isSafeInteger(preferred) || preferred<640))throw new Error('Invalid conversation width');
  const width=preferred===null?Math.max(680,Math.min(880,column*.64)):Math.min(preferred,Math.max(640,column-176));
  return Math.min(column,width);
}
