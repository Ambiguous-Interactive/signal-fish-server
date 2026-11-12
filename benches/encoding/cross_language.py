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


def run(command, cwd=ROOT, capture=None):
    result = subprocess.run(command, cwd=cwd, text=True, capture_output=True, timeout=600)
    if capture is not None:
        capture.write_text(result.stdout)
        capture.with_suffix('.stderr.txt').write_text(result.stderr)
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


def logical_equal(left, right):
    # Equal integer/decimal coordinate spellings are allowed. JSON booleans
    # are a separate type even though Python bool inherits from int.
    if isinstance(left, bool) or isinstance(right, bool):
        return type(left) is type(right) and left == right
    if isinstance(left, (int, float)) and isinstance(right, (int, float)):
        return left == right
    if type(left) is not type(right):
        return False
    if isinstance(left, dict):
        return left.keys() == right.keys() and all(logical_equal(left[key], right[key]) for key in left)
    if isinstance(left, list):
        return len(left) == len(right) and all(logical_equal(a, b) for a, b in zip(left, right))
    return left == right


def verify_order(row, fixture_index):
    context = f"{row['language']}/{row['codec']}/{row['fixture']}"
    modes = MODES[row['language']]
    benchmark = row.get('benchmark')
    require(isinstance(benchmark, dict), f'{context}: missing order evidence')
    require(benchmark.get('warmup_ms') == 1000, f'{context}: warmup differs')
    if len(modes) == 1:
        require(benchmark.get('method') == 'single-codec' and benchmark.get('seed') is None,
                f'{context}: single-codec baseline differs')
        first = modes
    else:
        seed = benchmark.get('seed')
        require(type(seed) is int and 0 <= seed <= 0xffffffff, f'{context}: invalid seed')
        require(benchmark.get('method') == 'seeded-alternating', f'{context}: invalid order method')
        first = modes if (seed + fixture_index) % 2 == 0 else list(reversed(modes))
    orders = [first if index % 2 == 0 else list(reversed(first)) for index in range(10)]
    require(benchmark.get('warmup_order') == first, f'{context}: warmup order differs')
    require(benchmark.get('sample_orders') == orders, f'{context}: sample order differs')
    for operation in ('encode', 'decode'):
        sample = row[operation]
        require(sample.get('order_positions') == [order.index(row['codec']) for order in orders],
                f'{context}/{operation}: recorded execution position differs')
        require(type(sample.get('warmup_iterations')) is int and sample['warmup_iterations'] > 0,
                f'{context}/{operation}: missing warmup count')
        require(isinstance(sample.get('warmup_ns'), (int, float)) and
                math.isfinite(sample['warmup_ns']) and sample['warmup_ns'] >= 1e9,
                f'{context}/{operation}: incomplete warmup')


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
    seeds = set()
    for row in rows:
        fixture = fixtures[row['fixture']]
        source = (CORPUS / fixture['file']).read_bytes()
        require(len(source) == fixture['bytes'] and
                hashlib.sha256(source).hexdigest() == fixture['sha256'], 'Source manifest differs')
        value = json.loads(source)
        context = f"{row['language']}/{row['codec']}/{row['fixture']}"
        require(logical_equal(row['logical'], value), f'{context}: logical values differ')
        require(row['float_bits'] == bits(value), f'{context}: floating-point bits differ')
        wire = base64.b64decode(row['wire_base64'], validate=True)
        require(len(wire) == row['bytes'], f'{context}: wire byte count differs')
        if row['codec'] in ('protobuf', 'schema-varint'):
            codec = 'prost' if row['codec'] == 'protobuf' else 'schema-varint'
            require(wire == rust_wires[(row['fixture'], codec)], f'{context}: Rust wire differs')
        if measured:
            verify_order(row, list(fixtures).index(row['fixture']))
            if row['language'] != 'csharp':
                seeds.add(row['benchmark']['seed'])
        else:
            require(row.get('benchmark') is None, f'{context}: check-only run contains order evidence')
        for operation in ('encode', 'decode'):
            sample = row[operation]
            if measured:
                require(sample['iterations'] > 0 and len(sample['samples_ns']) == 10,
                        f'{context}/{operation}: incomplete samples')
                require(all(math.isfinite(n) and n > 0 for n in sample['samples_ns']),
                        f'{context}/{operation}: invalid timings')
            else:
                require(sample is None, f'{context}: check-only run contains timings')
    if measured:
        require(len(seeds) == 1, 'Harness seeds disagree')


def self_test_order(rows, rust):
    import copy
    controls = copy.deepcopy(rows)
    names = [fixture['name'] for fixture in json.loads((CORPUS / 'manifest.json').read_text())['fixtures']]
    for row in controls:
        modes = MODES[row['language']]
        first = modes if (1524 + names.index(row['fixture'])) % 2 == 0 else list(reversed(modes))
        orders = [first if index % 2 == 0 else list(reversed(first)) for index in range(10)]
        row['benchmark'] = {'method': 'single-codec' if len(modes) == 1 else 'seeded-alternating',
                            'seed': None if len(modes) == 1 else 1524, 'warmup_ms': 1000,
                            'warmup_order': first, 'sample_orders': orders}
        for operation in ('encode', 'decode'):
            row[operation] = {'iterations': 1, 'warmup_iterations': 1, 'warmup_ns': 1e9,
                              'samples_ns': [1.0] * 10, 'checksum': 1,
                              'order_positions': [order.index(row['codec']) for order in orders]}
    verify(controls, rust, True)
    for mutation in ('missing', 'fixed', 'seed', 'seed_disagreement', 'warmup_order', 'position', 'warmup_time', 'warmup_count', 'samples'):
        broken = copy.deepcopy(controls)
        row = next(row for row in broken if row['language'] == 'typescript')
        if mutation == 'missing':
            row.pop('benchmark')
        elif mutation == 'fixed':
            row['benchmark']['sample_orders'] = [row['benchmark']['sample_orders'][0]] * 10
        elif mutation == 'seed':
            row['benchmark']['seed'] += 1
        elif mutation == 'seed_disagreement':
            row['benchmark']['seed'] += 2
        elif mutation == 'warmup_order':
            row['benchmark']['warmup_order'].reverse()
        elif mutation == 'position':
            row['encode']['order_positions'][0] ^= 1
        elif mutation == 'warmup_time':
            row['decode']['warmup_ns'] = 999999999
        elif mutation == 'warmup_count':
            row['encode']['warmup_iterations'] = 0
        else:
            row['decode']['samples_ns'].pop()
        try:
            verify(broken, rust, True)
        except ValueError:
            continue
        raise ValueError(f'Order oracle accepted {mutation} mutation')


def self_test(rows, rust):
    # Independent negative controls must reject both value and signed-zero loss.
    import copy
    for mutate in ('value', 'boolean', 'bits', 'missing', 'duplicate', 'wire'):
        broken = copy.deepcopy(rows)
        if mutate == 'value':
            broken[0]['logical'][0] += 1
        elif mutate == 'boolean':
            record = next(r for r in broken if r['fixture'] == 'snapshot-small')
            record['logical']['entities'][0]['active'] = 0
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
    self_test_order(rows, rust)
    require(all(row['benchmark']['seed'] == environment['sample_order_seed'] for row in rows
                if row['language'] != 'csharp'), 'Recorded seed differs')
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
    parser.add_argument('--seed', type=int, default=1524)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--verify-recorded', type=Path)
    args = parser.parse_args()
    if args.verify_recorded:
        verify_saved(args.verify_recorded)
        print('Recorded cells, samples, summary and source manifest agree.')
        return
    require(args.check or args.output is not None, 'Measured runs require --output')
    require(0 <= args.seed <= 0xffffffff, 'Seed must fit uint32')
    suffix = ['--check'] if args.check else []
    ordered_suffix = [*suffix, f'--seed={args.seed}']
    if args.output is not None:
        args.output.mkdir(parents=True, exist_ok=False)
    def capture(name):
        return args.output / name if args.output is not None else None
    run(['npm', 'run', 'check'], STUDY / 'typescript')
    run([args.dotnet, 'restore', str(STUDY / 'csharp'), '--locked-mode'])
    run([args.dotnet, 'build', str(STUDY / 'csharp'), '-c', 'Release', '--no-restore'])
    rows = records(run(['npm', 'exec', '--', 'tsx', 'harness.ts', *ordered_suffix],
                       STUDY / 'typescript', capture('typescript.jsonl')))
    rows.extend(records(run([args.dotnet, str(STUDY / 'csharp/bin/Release/net10.0/EncodingStudy.dll'), str(ROOT), *suffix],
                            capture=capture('csharp.jsonl'))))
    with tempfile.TemporaryDirectory() as temporary:
        output = Path(temporary) / 'godot.jsonl'
        try:
            run([args.godot, '--quiet', '--headless', '--path', str(STUDY / 'godot'), '--script', 'harness.gd',
                 '--', str(ROOT), str(output), *ordered_suffix], capture=capture('godot-process.txt'))
        finally:
            if output.exists() and args.output is not None:
                (args.output / 'godot.jsonl').write_text(output.read_text())
        rows.extend(records(output.read_text()))
    rust = records(run(['cargo', 'run', '--quiet', '--locked', '--manifest-path',
                        str(STUDY / 'Cargo.toml'), '--example', 'wire']))
    verify(rows, rust, not args.check)
    self_test(rows, rust)
    self_test_order(rows, rust)
    if not args.check:
        require(all(row['benchmark']['seed'] == args.seed for row in rows
                    if row['language'] != 'csharp'), 'Harness seed differs from requested seed')
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
        environment = {'sample_order_seed': args.seed, 'sample_order_method': 'seeded-alternating', 'utc': datetime.now(timezone.utc).isoformat(), 'host': platform.platform(),
                       'machine': platform.machine(), 'cpu': Path('/proc/cpuinfo').read_text() if Path('/proc/cpuinfo').exists() else None,
                       'node': run(['node', '--version']).strip(), 'dotnet': run([args.dotnet, '--info']),
                       'godot': run([args.godot, '--version']).strip(),
                       'cpu_max': Path('/sys/fs/cgroup/cpu.max').read_text().strip() if Path('/sys/fs/cgroup/cpu.max').exists() else None,
                       'memory_max': Path('/sys/fs/cgroup/memory.max').read_text().strip() if Path('/sys/fs/cgroup/memory.max').exists() else None,
                       'sources': {name: hashlib.sha256((STUDY / name).read_bytes()).hexdigest() for name in sorted(sources)},
                       'corpus_manifest_sha256': hashlib.sha256((CORPUS / 'manifest.json').read_bytes()).hexdigest()}
        (args.output / 'environment.json').write_text(json.dumps(environment, indent=2) + '\n')
    print(f'Passed {len(rows)} cells, 8 Rust reference wires, and 15 negative controls.')


if __name__ == '__main__':
    main()
