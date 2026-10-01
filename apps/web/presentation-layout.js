// Device layout only. Submission durability belongs to drafts.js and Rust.
export const defaultLayout = Object.freeze({version:3,navigationWidth:280,resourcesWidth:.45,navigation:'expanded',resourcesClosed:true,detail:'standard',workspaces:[]});
export function parseLayout(value) {
  const fallback = () => {throw new Error('Invalid layout');};
  if (!value || typeof value !== 'object' || Array.isArray(value)) return fallback();
  if (Object.keys(value).sort().join() !== Object.keys(defaultLayout).sort().join() || value.version!==3 ||
      !Number.isFinite(value.navigationWidth) || !Number.isFinite(value.resourcesWidth) ||
      !['expanded','rail','hidden'].includes(value.navigation) || typeof value.resourcesClosed!=='boolean' ||
      !['compact','standard','detailed','verbose'].includes(value.detail) || !Array.isArray(value.workspaces) || value.workspaces.length>16) return fallback();
  const ids=new Set();
  for(const item of value.workspaces) {
    if (!item || Object.keys(item).sort().join()!=='expanded,id' || typeof item.id!=='string' || !/^[a-zA-Z0-9_-]{1,128}$/.test(item.id) || typeof item.expanded!=='boolean' || ids.has(item.id)) return fallback();
    ids.add(item.id);
  }
  return {...value,navigationWidth:Math.round(Math.max(264,Math.min(420,value.navigationWidth))),resourcesWidth:Math.max(.2,Math.min(.7,value.resourcesWidth)),workspaces:value.workspaces.map(item=>({...item}))};
}
export function workspacePreference(layout,id,expanded) {
  return validateLayout({...layout,workspaces:[...layout.workspaces.filter(item=>item.id!==id),{id,expanded}].slice(-16)});
}

export function validateLayout(value) {
  try {return parseLayout(value);} catch {return structuredClone(defaultLayout);}
}
