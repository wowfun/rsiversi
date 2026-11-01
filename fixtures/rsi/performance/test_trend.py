import copy
import io
import json
from pathlib import Path
import tempfile
import subprocess
import unittest
from unittest.mock import patch
import zipfile
import trend
import test_evidence


class Trend(unittest.TestCase):
    def pair(self, surface='gui'):
        metadata = dict(format=1, recipe=trend.RECIPE, surface=surface, build='release', hardware={'cpu':'fixture','available_cpus':2}, rust='fixture', cargo='fixture', commit='a', results=[{'exit_code':0}])
        return [metadata, dict(format=1, compatibility=trend.compatibility(metadata), metrics={'work':trend.distribution([1,2,3])})]

    def test_compatible_commit_changes_and_relative_absolute_deltas(self):
        baseline=self.pair(); current=copy.deepcopy(baseline)
        current[0]['commit']='b'; current[1]['metrics']['work']=trend.distribution([2,4,6])
        delta=trend.compare(current,baseline)['deltas']['work']
        self.assertEqual(delta['absolute'],2); self.assertEqual(delta['relative_percent'],100)

    def test_incompatible_and_missing_measurements_are_rejected(self):
        for key in ('recipe','build','hardware','rust','cargo','surface'):
            pair=self.pair(); pair[0][key]='different'
            with self.assertRaises(ValueError): trend.compare(self.pair(),pair)
        pair=self.pair(); pair[1]['metrics']={}
        with self.assertRaises(ValueError): trend.compare(self.pair(),pair)

    def test_archive_admission_is_exact_and_bounded(self):
        def archive(names, payload=None):
            stream=io.BytesIO()
            with zipfile.ZipFile(stream,'w',zipfile.ZIP_DEFLATED) as target:
                for index,name in enumerate(names): target.writestr(name,payload or json.dumps(self.pair()[index]))
            return stream.getvalue()
        self.assertEqual(trend.read_archive(archive(['metadata.json','summary.json'])),self.pair())
        for names,payload in [(['../metadata.json','summary.json'],None),(['metadata.json','metadata.json'],None),(['metadata.json','summary.json'],'x'*trend.LIMIT)]:
            with self.assertRaises(ValueError): trend.read_archive(archive(names,payload))
        with self.assertRaises(ValueError): trend.read_archive(b'0'*(trend.LIMIT+1))

    def test_all_existing_owner_formats_produce_distributions(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp); test_evidence.Evidence().valid_reports(root)
            for surface in ('gui','store','tui'):
                summary=trend.summarize(root,self.pair(surface)[0])
                self.assertTrue(summary['metrics'])
                self.assertTrue(all('median' in row for row in summary['metrics'].values()))
                if surface == 'store':
                    cold=[row for key,row in summary['metrics'].items() if '/cold_new_inode/' in key]
                    self.assertEqual(len(cold),24)
                    self.assertTrue(all(row['count']==1 for row in cold))

    def test_lookup_wall_clock_timeout_is_explicit(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            for name,value in zip(('metadata.json','summary.json'),self.pair()):
                (root/name).write_text(json.dumps(value))
            with patch('sys.argv',['trend.py',str(root),'--github']), patch.dict('os.environ',{},clear=True), patch('trend.subprocess.run',side_effect=subprocess.TimeoutExpired('fixture',180)) as child:
                trend.main()
                self.assertEqual(child.call_args.kwargs['timeout'],180)
            self.assertEqual(json.loads((root/'comparison.json').read_text())['status'],'lookup_failed')

    def test_missing_local_baseline_is_not_a_remote_lookup_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            for name,value in zip(('metadata.json','summary.json'),self.pair()):
                (root/name).write_text(json.dumps(value))
            with patch('sys.argv',['trend.py',str(root),'--baseline',str(root/'missing')]), patch.dict('os.environ',{},clear=True):
                trend.main()
            self.assertEqual(json.loads((root/'comparison.json').read_text())['status'],'baseline_rejected')

    def test_operational_cases_keep_distinct_measurements_and_reject_overwrite(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary); test_evidence.Evidence().valid_reports(root)
            path=root/'probe-0.log'
            content=path.read_text().replace('operational_one_fact_page min_ns=0 median_ns=0 p95_ns=0','operational_one_fact_page min_ns=2 median_ns=257 p95_ns=300',1)
            content=content.replace('operational_one_fact_page min_ns=0 median_ns=0 p95_ns=0','operational_one_fact_page min_ns=4 median_ns=512 p95_ns=600',1)
            path.write_text(content)
            metrics=trend.summarize(root,self.pair('store')[0])['metrics']
            operational={key:row for key,row in metrics.items() if key.endswith('/operational_one_fact_page')}
            self.assertEqual(len(operational),2)
            for count in (257,512):
                rows=[row for key,row in operational.items() if key.startswith(f'operational_case sessions={count} ')]
                self.assertEqual([row['median'] for row in rows],[count])
            path.write_text(content+'\nmetadata_case sessions=512 samples=100\nreopened_header min_ns=1 median_ns=1 p95_ns=1\n')
            with self.assertRaisesRegex(ValueError,'Duplicate'):
                trend.summarize(root,self.pair('store')[0])

    def test_operational_matrix_is_required_before_summarizing(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary); test_evidence.Evidence().valid_reports(root)
            path=root/'probe-0.log'; lines=path.read_text().splitlines()
            path.write_text('\n'.join(line for line in lines if not line.startswith(('operational_case ','operational_one_fact_page '))))
            with self.assertRaises(ValueError): test_evidence.run.validate_store(root)

    def test_missing_history_is_not_a_no_regression_claim(self):
        class Response:
            def __enter__(self): return self
            def __exit__(self,*args): pass
            def read(self,limit): return b'{"workflow_runs":[]}'
        with patch('trend.urllib.request.build_opener') as opener:
            opener.return_value.open.return_value=Response()
            result=trend.lookup(self.pair(),dict(GITHUB_REPOSITORY='owner/repo',RSI_DEFAULT_BRANCH='main',GITHUB_RUN_ID='5',GH_TOKEN='fixture'))
            self.assertEqual(result['status'],'baseline_unavailable')
