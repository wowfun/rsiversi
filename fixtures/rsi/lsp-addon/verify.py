"""Run the independent public-SDK launcher against four real read-only queries."""
import argparse, importlib.util, json, subprocess
from pathlib import Path
parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,required=True);parser.add_argument('--analyzer',type=Path,required=True);parser.add_argument('--report',type=Path,required=True);args=parser.parse_args()
args.report.mkdir(parents=True);workspace=args.report/'workspace';workspace.mkdir();host=args.report/'fixture.toml';host.write_text('format=1\nsteps=[]\n')
spec=importlib.util.spec_from_file_location('language_fixture',Path(__file__).parent.parent/'lsp/prepare.py');fixture=importlib.util.module_from_spec(spec);spec.loader.exec_module(fixture)
position=fixture.prepare(workspace,host,args.analyzer);source=(workspace/'src/main.rs').read_bytes();results=[]
for operation,line,column in [('definition',position['line'],position['column']),('references',2,12),('implementation',1,11),('hover',position['line'],position['column'])]:
    query={'operation':operation,'path':'src/main.rs','line':line,'column':column}
    process=subprocess.run([str(args.binary.resolve()),str(workspace/'language-config.json'),str(workspace),json.dumps(query),'--wait-for-result'],capture_output=True,text=True,timeout=50)
    (args.report/(operation+'.stderr')).write_text(process.stderr)
    assert process.returncode==0,process.stderr
    output=json.loads(process.stdout);assert output['query']==query
    result=output['result']
    if operation=='hover': assert 'Bird' in result['text'],result
    else:
        assert result['locations'],result
        assert all(item['path']=='src/main.rs' for item in result['locations'])
        if operation=='definition': assert result['locations'][0]['range']['start']=={'line':1,'character':11}
        if operation=='references': assert len(result['locations'])>=3
        if operation=='implementation': assert result['locations'][0]['range']['start']['line']==2
    results.append(output)
assert (workspace/'src/main.rs').read_bytes()==source
(args.report/'result.json').write_text(json.dumps({'status':'passed','server':position['version'],'public_launcher':True,'queries':results},ensure_ascii=False,indent=2))
