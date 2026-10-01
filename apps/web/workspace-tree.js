// Presentation projection only; coordinates arrive from the workspace owner.
export function workspaceTree(workspaces,order=[]) {
  const ranks=new Map(order.map((id,index)=>[id,index]));
  const locations=new Map();
  for(const workspace of workspaces){
    const key=workspace.location.kind==='local'?'local':`ssh:${workspace.location.target}`;
    let location=locations.get(key);
    if(!location){location={key,label:key==='local'?'Local':`SSH ${workspace.location.target}`,children:[],nodes:new Map()};locations.set(key,location);}
    const parts=workspace.path.split(workspace.path.startsWith('/')?'/':/[\\/]/).filter(Boolean);
    let children=location.children,prefix='';
    for(const [index,label] of parts.entries()){
      prefix+=`/${label}`;
      let node=location.nodes.get(prefix);
      if(!node){node={key:`${key}:${prefix}`,label,children:[],workspace:null};location.nodes.set(prefix,node);children.push(node);}
      if(index===parts.length-1)node.workspace=workspace;
      children=node.children;
    }
    if(!parts.length)location.children.push({key:`${key}:/`,label:'/',children:[],workspace});
  }
  const sort=nodes=>{nodes.sort((a,b)=>a.label.localeCompare(b.label)||a.key.localeCompare(b.key));const positions=nodes.flatMap((node,index)=>node.workspace?[index]:[]);const siblings=positions.map(index=>nodes[index]).sort((a,b)=>(ranks.get(a.workspace.id)??Number.MAX_SAFE_INTEGER)-(ranks.get(b.workspace.id)??Number.MAX_SAFE_INTEGER));positions.forEach((index,offset)=>nodes[index]=siblings[offset]);for(const node of nodes)sort(node.children);};
  return [...locations.values()].sort((a,b)=>a.key.localeCompare(b.key)).map(({key,label,children})=>{sort(children);return {key,label,children};});
}

export function workspaceMembers(workspaces) {
  const parents=new Map();
  return workspaces.map(workspace=>{
    const path=workspace.path.startsWith('/')?workspace.path:workspace.path.replaceAll('\\','/'),parent=path.slice(0,path.lastIndexOf('/'));
    const key=JSON.stringify([workspace.location,parent]);
    if(!parents.has(key))parents.set(key,parents.size);
    return {id:workspace.id,partition:String(parents.get(key))};
  });
}
