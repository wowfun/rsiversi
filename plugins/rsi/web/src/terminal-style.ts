// xterm's public documentOverride confines this adapter to one renderer.
// Constructed CSSOM sheets preserve style-src 'self'; no global DOM patch or
// HTML from terminal output is admitted. Only xterm's bounded generated CSS enters.
export function terminalDocument(owner:Document) {
  const sheets=new Set<CSSStyleSheet>()
  const remove=(sheet:CSSStyleSheet)=>{
    owner.adoptedStyleSheets=owner.adoptedStyleSheets.filter(value=>value!==sheet)
    sheets.delete(sheet)
  }
  const document=new Proxy(owner,{
    get(target,key){
      if(key==='createElement')return (name:string,options?:ElementCreationOptions)=>{
        // Even an empty style node with an inert MIME type triggers CSP checks
        // in Chromium. xterm needs only textContent/remove, so use a template.
        const element=target.createElement(name.toLowerCase()==='style'?'template':name,options)
        if(name.toLowerCase()!=='style')return element
        const sheet=new CSSStyleSheet()
        let text=''
        Object.defineProperty(element,'textContent',{
          get:()=>text,
          set:(value:string)=>{
            if(value.length>256*1024)throw new Error('Terminal stylesheet exceeds its limit')
            sheet.replaceSync(value);text=value
            if(!sheets.has(sheet)){sheets.add(sheet);owner.adoptedStyleSheets=[...owner.adoptedStyleSheets,sheet]}
          },
        })
        const detach=element.remove.bind(element)
        element.remove=()=>{remove(sheet);detach()}
        return element
      }
      const value=Reflect.get(target,key,target)
      return typeof value==='function'?value.bind(target):value
    },
  })
  return {document,dispose:()=>{for(const sheet of sheets)remove(sheet)}}
}
