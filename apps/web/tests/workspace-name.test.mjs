import {test} from 'node:test';
import assert from 'node:assert/strict';
import {workspaceName} from '../workspace-name.js';

test('workspace display names preserve roots, normalize separators and prefer custom names',()=>{
  for(const [path,name] of [['/project/', 'project'],['C:\\projects\\工作区\\','工作区'],['C:/projects/workspace///','workspace'],['/', '/'],['C:\\','C:\\'],['/Documents/RSIversi/default-workspace/','Default workspace'],['','']]) {
    assert.equal(workspaceName(path),name);
    assert.equal(workspaceName(path,'Named workspace'),'Named workspace');
  }
});
