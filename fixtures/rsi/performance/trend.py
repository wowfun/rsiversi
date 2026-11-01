"""Bounded, report-only comparison of compatible performance evidence."""
import argparse
from collections import defaultdict
import io
import json
import math
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import sys
import time
import urllib.request
import urllib.parse
import zipfile

LIMIT = 1024 * 1024
RECIPE = 2


def hardware():
    cpu = platform.processor()
    if sys.platform == 'win32':
        import winreg
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, r'HARDWARE\DESCRIPTION\System\CentralProcessor\0') as key:
            cpu = winreg.QueryValueEx(key, 'ProcessorNameString')[0].strip()
    if Path('/proc/cpuinfo').is_file():
        match = re.search(r'^model name\s*:\s*(.+)$', Path('/proc/cpuinfo').read_text(), re.M)
        if match:
            cpu = match[1]
    available = len(os.sched_getaffinity(0)) if hasattr(os, 'sched_getaffinity') else getattr(os, 'process_cpu_count', os.cpu_count)()
    return dict(os=platform.system(), release=platform.release(), arch=platform.machine(), cpu=cpu or 'unknown', available_cpus=available)


def compatibility(metadata):
    return {key: metadata[key] for key in ('format', 'recipe', 'surface', 'build', 'hardware', 'rust', 'cargo')}


def distribution(samples):
    if not samples or any(type(n) not in (int, float) or not math.isfinite(n) or n < 0 for n in samples):
        raise ValueError('Invalid samples')
    ordered = sorted(samples)
    return dict(count=len(samples), min=ordered[0], median=statistics.median(ordered), p95=ordered[math.ceil(len(ordered)*.95)-1], max=ordered[-1])


def summarize(output, metadata):
    groups = defaultdict(list)
    def add(prefix, row, identity=()):
        for key, value in row.items():
            if key in identity or key in ('run', 'sample'):
                continue
            label = prefix + '/' + key
            if isinstance(value, dict):
                add(label, value)
            elif type(value) in (int, float):
                groups[label].append(value)
            elif isinstance(value, list) and all(type(n) in (int, float) for n in value):
                groups[label].extend(value)
    surface = metadata['surface']
    if surface == 'gui':
        for index, marker, identity in [(0, 'PROJECTION_PERF ', ('blocks',)), (1, 'global_frame ', ('detail_bytes', 'scenario'))]:
            for line in (output / f'probe-{index}.log').read_text(encoding='utf8').splitlines():
                if marker in line:
                    row = json.loads(line.split(marker, 1)[1])
                    add(marker.strip() + '/' + '/'.join(f'{k}={row[k]}' for k in identity), row, identity)
    elif surface == 'tui':
        for row in json.loads((output / 'cells.json').read_text())['runs']:
            add(f"blocks={row['blocks']}", row, ('blocks',))
    else:
        for path in sorted((output / 'store').glob('*.json')):
            for lane, rows in json.loads(path.read_text()).items():
                if isinstance(rows, list):
                    for row in rows:
                        add(path.stem + '/' + lane, row)
        # The owning Store log exposes aggregate distributions, not raw samples.
        case = ''
        aggregates = {}
        for line in (output / 'probe-0.log').read_text().splitlines():
            if re.match(r'^(case |metadata_case |control_case |operational_case )', line):
                case = re.sub(r' (db_bytes|.*_ns)=.*', '', line)
            match = re.fullmatch(r'(\w+) (?:samples=(\d+) )?min_ns=(\d+) median_ns=(\d+) (max_ns|p95_ns)=(\d+)', line)
            if match and case:
                label, count, low, median, upper, high = match.groups()
                key = case + '/' + label
                if key in aggregates:
                    raise ValueError('Duplicate Store aggregate: ' + key)
                aggregates[key] = dict(count=int(count) if count else None, min=int(low), median=int(median), **{upper[:-3]: int(high)})
            cold = re.fullmatch(r'cold_new_inode open_ns=(\d+) first_validation_ns=(\d+) verify_ns=(\d+)', line)
            if cold and case:
                add(case + '/cold_new_inode', dict(zip(('open_ns', 'first_validation_ns', 'verify_ns'), map(int, cold.groups()))))
    result = dict(format=1, compatibility=compatibility(metadata), metrics={key: distribution(values) for key, values in sorted(groups.items())})
    if surface == 'store':
        result['metrics'].update(aggregates)
    if not result['metrics']:
        raise ValueError('No measurements to summarize')
    (output / 'summary.json').write_text(json.dumps(result, indent=2), encoding='utf8')
    return result


def read_pair(directory):
    values = []
    for name in ('metadata.json', 'summary.json'):
        with (directory / name).open('rb') as source:
            data = source.read(LIMIT + 1)
        if len(data) > LIMIT:
            raise ValueError('Oversized summary')
        values.append(json.loads(data))
    return values


def read_archive(data):
    if len(data) > LIMIT:
        raise ValueError('Oversized compressed artifact')
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        members = archive.infolist()
        if len(members) != 2 or {m.filename for m in members} != {'metadata.json', 'summary.json'} or sum(m.file_size for m in members) > LIMIT:
            raise ValueError('Invalid summary archive members or expanded size')
        return [json.loads(archive.read(name)) for name in ('metadata.json', 'summary.json')]


def compare(current, candidate):
    cm, cs = current
    bm, bs = candidate
    if bs.get('format') != 1 or compatibility(cm) != compatibility(bm) or cs['compatibility'] != bs.get('compatibility') or bs['compatibility'] != compatibility(bm):
        raise ValueError('Incompatible provenance')
    if not bm.get('results') or any(r.get('exit_code') != 0 or r.get('evidence_error') for r in bm['results']):
        raise ValueError('Baseline probes did not pass')
    if set(cs['metrics']) != set(bs.get('metrics', {})):
        raise ValueError('Scenario matrix differs')
    deltas = {}
    for key, row in cs['metrics'].items():
        previous = bs['metrics'][key]
        for record in (row, previous):
            if any(type(record.get(k)) not in (int, float) or not math.isfinite(record[k]) or record[k] < 0 for k in ('min', 'median')):
                raise ValueError('Invalid distribution')
        delta = row['median'] - previous['median']
        deltas[key] = dict(current=row, baseline=previous, absolute=delta, relative_percent=100*delta/previous['median'] if previous['median'] else None)
    return dict(status='compared', baseline_commit=bm['commit'], deltas=deltas)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def lookup(current, environment=os.environ):
    deadline = time.monotonic() + 180
    repo, branch, run_id = (environment[k] for k in ('GITHUB_REPOSITORY', 'RSI_DEFAULT_BRANCH', 'GITHUB_RUN_ID'))
    api = 'https://api.github.com/repos/' + repo
    opener = urllib.request.build_opener(NoRedirect())
    def get(url, authenticated=True, binary=False):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError('Baseline lookup budget exhausted')
        headers = {'Accept': 'application/vnd.github+json'}
        if authenticated:
            headers['Authorization'] = 'Bearer ' + environment['GH_TOKEN']
        request = urllib.request.Request(url, headers=headers)
        try:
            response = opener.open(request, timeout=min(15, remaining))
        except urllib.error.HTTPError as error:
            if binary and authenticated and error.code in (301, 302, 303, 307, 308):
                destination = error.headers['Location']
                if urllib.parse.urlparse(destination).scheme != 'https':
                    raise ValueError('Non-HTTPS artifact redirect')
                return get(destination, authenticated=False, binary=True)
            raise
        with response:
            data = response.read(LIMIT + 1)
        if len(data) > LIMIT:
            raise ValueError('Oversized baseline response')
        return data if binary else json.loads(data)
    query = urllib.parse.urlencode(dict(branch=branch, status='success', per_page=10))
    runs = get(api + '/actions/workflows/performance-evidence.yml/runs?' + query)['workflow_runs']
    rejected = 0
    for run in runs[:10]:
        if str(run['id']) == run_id or run.get('head_branch') != branch or run.get('head_repository', {}).get('full_name') != repo or run.get('path') != '.github/workflows/performance-evidence.yml' or run.get('conclusion') != 'success':
            continue
        artifacts = get(api + f"/actions/runs/{run['id']}/artifacts?per_page=100")['artifacts']
        expected = f"performance-summary-{current[0]['surface']}-{run['head_sha']}"
        for artifact in artifacts[:100]:
            if artifact['name'] != expected or artifact['expired']:
                continue
            try:
                if artifact['size_in_bytes'] > LIMIT:
                    raise ValueError('Oversized artifact')
                pair = read_archive(get(api + f"/actions/artifacts/{artifact['id']}/zip", binary=True))
                if pair[0]['commit'] != run['head_sha']:
                    raise ValueError('Artifact commit mismatch')
                result = compare(current, pair)
                return dict(result, baseline_run=run['id'], rejected_candidates=rejected)
            except (ValueError, KeyError, TypeError, RuntimeError, zipfile.BadZipFile):
                rejected += 1
    return dict(status='baseline_rejected' if rejected else 'baseline_unavailable', rejected_candidates=rejected)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('--baseline', type=Path)
    parser.add_argument('--github', action='store_true')
    parser.add_argument('--lookup', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    current = read_pair(args.output)
    if args.lookup:
        print(json.dumps(lookup(current)))
        return
    try:
        if args.baseline:
            result = compare(current, read_pair(args.baseline))
        elif args.github:
            # Bound the entire network operation, including a slow-trickling body.
            completed = subprocess.run([sys.executable, str(Path(__file__).resolve()), str(args.output.resolve()), '--lookup'], capture_output=True, timeout=180, check=True)
            if len(completed.stdout) > LIMIT:
                raise ValueError('Oversized lookup result')
            result = json.loads(completed.stdout)
        else:
            result = dict(status='baseline_unavailable')
    except (ValueError, KeyError, TypeError, RuntimeError, zipfile.BadZipFile):
        result = dict(status='baseline_rejected')
    except (OSError, TimeoutError, subprocess.SubprocessError):
        result = dict(status='baseline_rejected' if args.baseline else 'lookup_failed')
    (args.output / 'comparison.json').write_text(json.dumps(result, indent=2), encoding='utf8')
    lines = [f"Performance comparison: **{result['status']}**", '', '| Scenario | Current distribution | Baseline distribution | Median delta | Relative |', '|---|---|---|---:|---:|']
    for key, row in result.get('deltas', {}).items():
        relative = 'undefined (zero baseline)' if row['relative_percent'] is None else f"{row['relative_percent']:+.2f}%"
        lines.append(f"| {key} | {row['current']} | {row['baseline']} | {row['absolute']:+g} | {relative} |")
    report = '\n'.join(lines) + '\n'
    (args.output / 'comparison.md').write_text(report, encoding='utf8')
    if os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(os.environ['GITHUB_STEP_SUMMARY'], 'a', encoding='utf8') as target:
            target.write(report)


if __name__ == '__main__':
    main()
