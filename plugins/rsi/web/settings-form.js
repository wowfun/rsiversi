// Schema-directed input assistance. The registered Rust validator owns acceptance.
// JSON.parse would round large Rust integer values. Preserve their original JSON editor.
export function canUseSettingsForm(text) {
  for (const match of text.matchAll(/"(?:[^"\\]|\\.)*"|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/gs)) {
    if (match[1] && JSON.stringify(Number(match[1])) !== match[1]) return false;
    if (match[1] && /^-?\d+$/.test(match[1]) && (BigInt(match[1]) > BigInt(Number.MAX_SAFE_INTEGER) || BigInt(match[1]) < BigInt(Number.MIN_SAFE_INTEGER))) return false;
  }
  return true;
}
const own = (value,key) => Object.prototype.hasOwnProperty.call(value,key);
const node = (tag,text) => {const result=document.createElement(tag);if(text !== undefined) result.textContent=text;return result};
const simple = new Set(['type','title','description','default','properties','required','additionalProperties','enum','minimum','maximum','exclusiveMinimum','exclusiveMaximum','multipleOf','minLength','maxLength','pattern','$schema']);
const empty = schema => !schema || typeof schema !== 'object' ? null : own(schema,'default') ? structuredClone(schema.default) : ({object:{},string:'',boolean:false,number:0,integer:0}[schema.type] ?? null);
export function settingsForm(schema,value,disabled=false) {
  let count=0;
  function field(schema,value,label,depth) {
    const box=node('div');box.className='settings-field';
    const validSchema = schema && typeof schema === 'object' && !Array.isArray(schema);
    const supported = validSchema && Object.keys(schema).every(key=>simple.has(key)) && ++count<=64 && depth<=8;
    const title=validSchema && typeof schema.title === 'string' ? schema.title : label.split(' / ').at(-1).replaceAll('_',' ').replace(/^./,ch=>ch.toUpperCase());
    let read, json;
    if (supported && Array.isArray(schema.enum) && schema.enum.length>0 && schema.enum.length<=64 && schema.enum.every(item=>item===null || ['string','boolean','number'].includes(typeof item)) && schema.enum.some(item=>Object.is(item,value))) {
      const select=node('select');select.setAttribute('aria-label',label);
      for (const [index,item] of schema.enum.entries()) {const option=node('option',typeof item === 'string' ? item : JSON.stringify(item));option.value=String(index);option.selected=Object.is(item,value);select.append(option)}
      const caption=node('label',title);caption.append(select);box.append(caption);read=()=>schema.enum[Number(select.value)];
    } else if (supported && schema.type === 'object' && value && typeof value === 'object' && !Array.isArray(value) && schema.properties && typeof schema.properties === 'object' && !Array.isArray(schema.properties) && Object.keys(schema.properties).length<=32 && Object.keys(value).every(key=>own(schema.properties,key))) {
      const group=node('fieldset');group.className='settings-object';group.append(node('legend',title));
      const fields=node('div');fields.className='settings-fields';const parts=[];
      for (const [key,childSchema] of Object.entries(schema.properties)) {
        const child=field(childSchema,own(value,key) ? value[key] : empty(childSchema),`${label} / ${key}`,depth+1);
        const required=Array.isArray(schema.required) && schema.required.includes(key);
        const inclusion=node('input');inclusion.type='checkbox';inclusion.checked=own(value,key);inclusion.setAttribute('aria-label',`Include ${label} / ${key}`);
        const controls=node('fieldset');controls.disabled=!required && !inclusion.checked;controls.className='settings-field-controls';controls.hidden=!required && !inclusion.checked;controls.append(child.element);
        if (!required) {const caption=node('label');caption.className='optional';caption.append(inclusion,document.createTextNode(`Include ${key.replaceAll('_',' ')}`));fields.append(caption);inclusion.addEventListener('change',()=>{controls.disabled=!inclusion.checked;controls.hidden=!inclusion.checked})}
        fields.append(controls);parts.push({key,child,required,inclusion});
      }
      const included=()=>parts.filter(part=>part.required || part.inclusion.checked);
      group.append(fields);box.append(group);read=()=>Object.fromEntries(included().map(part=>[part.key,part.child.read()]));
      json=()=>`{${included().map(part=>`${JSON.stringify(part.key)}:${part.child.json()}`).join(',')}}`;
    } else if (supported && ['string','boolean','number','integer'].includes(schema.type) && typeof value === (schema.type === 'integer' ? 'number' : schema.type) && !schema.enum && !(schema.type === 'string' && /\r/.test(value))) {
      const input=node(schema.type === 'string' && /[\r\n]/.test(value) ? 'textarea' : 'input');input.setAttribute('aria-label',label);const caption=node('label',title);
      if (schema.type === 'boolean') {input.type='checkbox';input.checked=value;read=()=>input.checked}
      else if (schema.type === 'string') {
        if (input.tagName === 'INPUT') input.type='text';input.value=value;
        if (Number.isSafeInteger(schema.minLength) && schema.minLength>=0) input.minLength=schema.minLength;
        if (Number.isSafeInteger(schema.maxLength) && schema.maxLength>=0) input.maxLength=schema.maxLength;
        // JSON Schema and HTML use different regex dialects; server validation owns pattern.
        const initial=input.value;read=()=>input.value === initial ? value : input.value;
      } else {
        json=()=>{JSON.parse(input.value);return input.value};
        input.type='number';input.value=String(value);input.required=true;input.step=String(schema.multipleOf ?? (schema.type === 'integer' ? 1 : 'any'));
        if (typeof schema.minimum === 'number') input.min=String(schema.minimum);
        if (typeof schema.maximum === 'number') input.max=String(schema.maximum);
        read=()=>{if (!Number.isFinite(input.valueAsNumber)) throw new Error(`${label} must be a finite number`);if (schema.type === 'integer' && !Number.isSafeInteger(input.valueAsNumber)) throw new Error(`${label} exceeds the exact integer form range; use JSON`);if (!canUseSettingsForm(input.value)) throw new Error(`${label} requires the full JSON editor to preserve its exact number representation`);return input.valueAsNumber};
      }
      caption.append(input);box.append(caption);
    } else {
      const input=node('textarea');input.className='settings-text';input.value=JSON.stringify(value,null,2);input.spellcheck=false;input.setAttribute('aria-label',`${label} JSON`);const caption=node('label',`${title} (JSON)`);caption.append(input);box.append(caption);
      json=()=>{JSON.parse(input.value);return input.value};
      read=()=>{let value;try{value=JSON.parse(input.value)}catch{throw new Error(`${label} must contain valid JSON`)}if (!canUseSettingsForm(input.value)) throw new Error(`${label} requires the full JSON editor to preserve its exact numbers`);return value};
    }
    if (validSchema && typeof schema.description === 'string') {const hint=node('p',schema.description);hint.className='hint';box.append(hint)}
    return {element:box,read,json:json ?? (()=>JSON.stringify(read()))};
  }
  const fields=field(schema,value,'Settings',0),container=node('fieldset');container.disabled=disabled;container.className='settings-object';container.append(fields.element);
  return {element:container,read:fields.read,json:fields.json};
}
