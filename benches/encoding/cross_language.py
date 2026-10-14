#!/usr/bin/env python3
"""Run and independently verify every cross-language gameplay corpus cell."""
import argparse
import base64
import csv
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import statistics
import struct
import subprocess
import tempfile
from datetime import datetime, timezone

ROOT = Path(__file__).resolve().parents[2]
STUDY = ROOT / 'benches/encoding'
CORPUS = ROOT / 'test-fixtures/encoding'
MODES = {'typescript': ['msgpack-all-f64', 'protobuf'],
         'csharp': ['protobuf'], 'gdscript': ['msgpack-f32', 'schema-varint']}


def run(command, cwd=ROOT):
    result = subprocess.run(command, cwd=cwd, text=True, capture_output=True, timeout=600)
    if result.returncode:
        raise RuntimeError(f'{command} failed:\n{result.stdout}\n{result.stderr}')
    return result.stdout


def records(output):
    return [json.loads(line) for line in output.splitlines() if line.startswith('{')]


def bits(value):
    if isinstance(value, list):
        return [bit for item in value for bit in bits(item)]
    if isinstance(value, dict):
        return [bit for key, item in value.items() for bit in (
            [struct.pack('<d', float(number)).hex() for number in item]
            if key in ('position', 'velocity') else bits(item))]
    return []


def require(condition, message):
    if not condition:
        raise ValueError(message)


def verify(rows, rust, measured):
    manifest = json.loads((CORPUS / 'manifest.json').read_text())
    fixtures = {f['name']: f for f in manifest['fixtures']}
    expected = {(language, mode, name) for language, modes in MODES.items()
                for mode in modes for name in fixtures}
    actual = [(r['language'], r['codec'], r['fixture']) for r in rows]
    require(len(actual) == len(expected) and set(actual) == expected,
            'Missing, duplicate, or unexpected language/codec/fixture cells')
    rust_wires = {(r['fixture'], r['codec']): bytes(r['wire']) for r in rust}
    require(len(rust_wires) == 8 and len(rust) == 8, 'Incomplete Rust wire oracle')
    for row in rows:
        fixture = fixtures[row['fixture']]
        source = (CORPUS / fixture['file']).read_bytes()
        require(len(source) == fixture['bytes'] and
                hashlib.sha256(source).hexdigest() == fixture['sha256'], 'Source manifest differs')
        value = json.loads(source)
        context = f"{row['language']}/{row['codec']}/{row['fixture']}"
        require(row['logical'] == value, f'{context}: logical values differ')
        require(row['float_bits'] == bits(value), f'{context}: floating-point bits differ')
        wire = base64.b64decode(row['wire_base64'], validate=True)
        require(len(wire) == row['bytes'], f'{context}: wire byte count differs')
        if row['codec'] in ('protobuf', 'schema-varint'):
            codec = 'prost' if row['codec'] == 'protobuf' else 'schema-varint'
            require(wire == rust_wires[(row['fixture'], codec)], f'{context}: Rust wire differs')
        for operation in ('encode', 'decode'):
            sample = row[operation]
            if measured:
                require(sample['iterations'] > 0 and len(sample['samples_ns']) == 10,
                        f'{context}/{operation}: incomplete samples')
                require(all(math.isfinite(n) and n > 0 for n in sample['samples_ns']),
                        f'{context}/{operation}: invalid timings')
            else:
                require(sample is None, f'{context}: check-only run contains timings')


def self_test(rows, rust):
    # Independent negative controls must reject both value and signed-zero loss.
    import copy
    for mutate in ('value', 'bits', 'missing', 'duplicate', 'wire'):
        broken = copy.deepcopy(rows)
        if mutate == 'value':
            broken[0]['logical'][0] += 1
        elif mutate == 'bits':
            record = next(r for r in broken if '0000000000000080' in r['float_bits'])
            index = record['float_bits'].index('0000000000000080')
            record['float_bits'][index] = '0000000000000000'
        elif mutate == 'missing':
            broken.pop()
        elif mutate == 'duplicate':
            broken.append(broken[0])
        else:
            record = next(r for r in broken if r['codec'] == 'protobuf')
            wire = bytearray(base64.b64decode(record['wire_base64']))
            wire[0] ^= 1
            record['wire_base64'] = base64.b64encode(wire).decode()
        try:
            verify(broken, rust, broken[0]['encode'] is not None)
        except ValueError:
            continue
        raise ValueError(f'Independent oracle accepted {mutate} mutation')


def verify_saved(directory):
    rows = json.loads((directory / 'samples.json').read_text())
    rust = json.loads((directory / 'rust-wire.json').read_text())
    environment = json.loads((directory / 'environment.json').read_text())
    verify(rows, rust, True)
    self_test(rows, rust)
    for name, expected in environment['sources'].items():
        require(hashlib.sha256((STUDY / name).read_bytes()).hexdigest() == expected,
                f'Recorded source differs: {name}')
    require(hashlib.sha256((CORPUS / 'manifest.json').read_bytes()).hexdigest() ==
            environment['corpus_manifest_sha256'], 'Recorded corpus manifest differs')
    summaries = list(csv.DictReader((directory / 'summary.csv').open()))
    require(len(summaries) == len(rows), 'Recorded summary count differs')
    for row, summary in zip(rows, summaries):
        for key in ('language', 'codec', 'fixture'):
            require(summary[key] == row[key], f'Recorded summary {key} differs')
        require(int(summary['bytes']) == row['bytes'], 'Recorded summary bytes differ')
        for operation in ('encode', 'decode'):
            values = row[operation]['samples_ns']
            for label, expected in [('median', statistics.median(values)), ('min', min(values)), ('max', max(values))]:
                require(float(summary[f'{operation}_{label}_ns']) == expected,
                        f'Recorded summary {operation}/{label} differs')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dotnet', default='dotnet')
    parser.add_argument('--godot', default='godot')
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--verify-recorded', type=Path)
    args = parser.parse_args()
    if args.verify_recorded:
        verify_saved(args.verify_recorded)
        print('Recorded cells, samples, summary and source manifest agree.')
        return
    require(args.check or args.output is not None, 'Measured runs require --output')
    suffix = ['--check'] if args.check else []
    run(['npm', 'run', 'check'], STUDY / 'typescript')
    run([args.dotnet, 'restore', str(STUDY / 'csharp'), '--locked-mode'])
    run([args.dotnet, 'build', str(STUDY / 'csharp'), '-c', 'Release', '--no-restore'])
    rows = records(run(['npm', 'exec', '--', 'tsx', 'harness.ts', *suffix], STUDY / 'typescript'))
    rows.extend(records(run([args.dotnet, str(STUDY / 'csharp/bin/Release/net10.0/EncodingStudy.dll'), str(ROOT), *suffix])))
    with tempfile.TemporaryDirectory() as temporary:
        output = Path(temporary) / 'godot.jsonl'
        run([args.godot, '--quiet', '--headless', '--path', str(STUDY / 'godot'), '--script', 'harness.gd',
             '--', str(ROOT), str(output), *suffix])
        rows.extend(records(output.read_text()))
    rust = records(run(['cargo', 'run', '--quiet', '--locked', '--manifest-path',
                        str(STUDY / 'Cargo.toml'), '--example', 'wire']))
    verify(rows, rust, not args.check)
    self_test(rows, rust)
    if not args.check:
        args.output.mkdir(parents=True, exist_ok=True)
        (args.output / 'samples.json').write_text(json.dumps(rows, separators=(',', ':')) + '\n')
        (args.output / 'rust-wire.json').write_text(json.dumps(rust, separators=(',', ':')) + '\n')
        with (args.output / 'summary.csv').open('w') as file:
            writer = csv.writer(file)
            writer.writerow(['language', 'codec', 'fixture', 'bytes', 'encode_median_ns',
                             'encode_min_ns', 'encode_max_ns', 'decode_median_ns', 'decode_min_ns', 'decode_max_ns'])
            for row in rows:
                times = [number for operation in ('encode', 'decode') for number in (
                    statistics.median(row[operation]['samples_ns']),
                    min(row[operation]['samples_ns']), max(row[operation]['samples_ns']))]
                writer.writerow([row['language'], row['codec'], row['fixture'], row['bytes'], *times])
        # Fingerprint measured sources and dependency locks; exclude results/build products.
        sources = ['cross_language.py', 'schema/game.proto', 'examples/wire.rs', 'Cargo.toml', 'Cargo.lock']
        for directory in ['typescript', 'csharp', 'godot', 'support', 'generated']:
            sources.extend(str(p.relative_to(STUDY)) for p in (STUDY / directory).iterdir()
                           if p.is_file() and not p.name.endswith('.uid'))
        environment = {'utc': datetime.now(timezone.utc).isoformat(), 'host': platform.platform(),
                       'machine': platform.machine(), 'cpu': Path('/proc/cpuinfo').read_text() if Path('/proc/cpuinfo').exists() else None,
                       'node': run(['node', '--version']).strip(), 'dotnet': run([args.dotnet, '--info']),
                       'godot': run([args.godot, '--version']).strip(),
                       'cpu_max': Path('/sys/fs/cgroup/cpu.max').read_text().strip() if Path('/sys/fs/cgroup/cpu.max').exists() else None,
                       'memory_max': Path('/sys/fs/cgroup/memory.max').read_text().strip() if Path('/sys/fs/cgroup/memory.max').exists() else None,
                       'sources': {name: hashlib.sha256((STUDY / name).read_bytes()).hexdigest() for name in sorted(sources)},
                       'corpus_manifest_sha256': hashlib.sha256((CORPUS / 'manifest.json').read_bytes()).hexdigest()}
        (args.output / 'environment.json').write_text(json.dumps(environment, indent=2) + '\n')
    print(f'Passed {len(rows)} cells, 8 Rust reference wires, and 5 negative controls.')


if __name__ == '__main__':
    main()
