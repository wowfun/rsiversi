"""Capture actual native response policies and verify the application frame gate."""
import json

def verify(script, frame, until, report):
    script('''window.policyViolations=[];window.policyListener=e=>window.policyViolations.push(e.effectiveDirective);document.addEventListener('securitypolicyviolation',window.policyListener);const f=document.createElement('iframe');f.id='policy-blocked';f.src='https://foreign-preview.invalid/probe';document.body.append(f);return true''')
    until(lambda: script("return window.policyViolations.includes('frame-src')"))
    script("document.querySelector('#policy-blocked').remove();document.removeEventListener('securitypolicyviolation',window.policyListener);return true")
    responses = script("return Promise.all(['/', '/preview-local.html', '/preview-online.html'].map(async path=>{const response=await fetch(path);return {path,status:response.status,csp:response.headers.get('content-security-policy'),body:await response.text()}}))")
    assert all(response['status'] == 200 and response['csp'] for response in responses)
    (report / 'preview-responses.json').write_text(json.dumps(responses, indent=2))
    return {'frame_src_foreign_blocked': 'frame-src' in script('return window.policyViolations'), 'captured_response_policies': all(response['status'] == 200 and response['csp'] for response in responses)}
