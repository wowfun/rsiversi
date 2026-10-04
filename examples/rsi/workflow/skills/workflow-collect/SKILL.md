---
name: workflow-collect
description: Collect two local JSON numbers in parallel and verify their sum from the source files in a read-only Workflow.
---

Use this skill only in a Session whose sandbox is read-only. Ask the user to select
read-only before creating the Session if that prerequisite is missing. A prompt
asking children not to write is not enforcement.

For this tutorial, the workspace has left.json and right.json, each containing
an integer n. Call run_workflow once with background=true and the script below.
End the creator Turn when it detaches. On its completion notice, use workflow_read
to inspect the actual result and report its total. Never report success from the
invocation's detached receipt alone.

```javascript
const files = ['left.json', 'right.json'];
const schema = {type:'object',properties:{n:{type:'integer'}},required:['n'],additionalProperties:false};
await workflow.phase('Collect evidence', {files});
const rows = await workflow.parallel(files.map(file => () => workflow.agent({
  message:`Your only task is to read ${file} using file_read, then call report_result with direct arguments {n:actual_integer}. Do not call run_workflow, run_code, spawn_agent, or create child tasks. Do not modify files.`,
  output_schema:schema
})));
return await workflow.pipeline([async rows => {
  await workflow.phase('Verify sources', {count:rows.length});
  const fs = await import('node:fs/promises');
  for (let i=0;i<files.length;i++) {
    const source=JSON.parse(await fs.readFile(files[i],'utf8'));
    if (!Number.isInteger(source.n) || rows[i].value.n !== source.n) throw new Error('source mismatch');
  }
  return {total:rows.reduce((sum,row)=>sum+row.value.n,0),verified:true};
}], rows);
```
