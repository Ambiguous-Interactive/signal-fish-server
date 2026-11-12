#!/usr/bin/env python3
"""Check saved order cohorts independently of the benchmark timing code."""
import argparse
import copy
import csv
import json
import math
from pathlib import Path

SEEDS = (1524, 1525)
PAIRED_CODECS = {'typescript': ('msgpack-all-f64', 'protobuf'),
                 'gdscript': ('msgpack-f32', 'schema-varint')}
OPERATIONS = ('encode', 'decode')


def require(condition, message):
    if not condition:
        raise ValueError(message)


def verify(datasets, cohorts):
    fixtures = json.loads((Path(__file__).resolve().parents[2] /
                           'test-fixtures/encoding/manifest.json').read_text())['fixtures']
    expected = {(seed, language, codec, fixture['name'], operation)
                for seed in SEEDS for language, codecs in PAIRED_CODECS.items()
                for codec in codecs for fixture in fixtures for operation in OPERATIONS}
    calculated = {}
    require(set(datasets) == set(SEEDS), 'Missing or unexpected seed dataset')
    for seed, rows in datasets.items():
        for row in rows:
            if row['language'] == 'csharp':
                continue
            for operation in OPERATIONS:
                key = (seed, row['language'], row['codec'], row['fixture'], operation)
                require(key in expected and key not in calculated, f'Unexpected or duplicate sample cell: {key}')
                sample = row[operation]
                values, positions = sample['samples_ns'], sample['order_positions']
                require(len(values) == len(positions) == 10, f'Incomplete samples: {key}')
                require(all(type(p) is int and p in (0, 1) for p in positions), f'Invalid positions: {key}')
                require(all(isinstance(n, (float, int)) and math.isfinite(n) and n > 0 for n in values),
                        f'Invalid timing: {key}')
                medians = []
                for position in (0, 1):
                    ordered = sorted(n for n, p in zip(values, positions) if p == position)
                    require(len(ordered) == 5, f'Unbalanced order cohort: {key}')
                    medians.append(ordered[2])
                calculated[key] = (*medians, medians[1] / medians[0])
    require(set(calculated) == expected, 'Missing sample cells')
    observed = set()
    for row in cohorts:
        key = (int(row['seed']), row['language'], row['codec'], row['fixture'], row['operation'])
        require(key in expected and key not in observed, f'Unexpected or duplicate cohort: {key}')
        observed.add(key)
        require(int(row['first_count']) == int(row['second_count']) == 5, f'Cohort count differs: {key}')
        actual = tuple(float(row[name]) for name in ('first_median_ns', 'second_median_ns', 'second_over_first'))
        require(actual == calculated[key], f'Cohort values differ from raw samples: {key}')
    require(observed == expected, 'Missing cohort comparisons')
    return calculated


def self_test(datasets, cohorts):
    for mutation in ('missing', 'duplicate', 'median', 'ratio', 'sample', 'position'):
        broken_data, broken_cohorts = copy.deepcopy((datasets, cohorts))
        if mutation == 'missing':
            broken_cohorts.pop()
        elif mutation == 'duplicate':
            broken_cohorts.append(broken_cohorts[0])
        elif mutation == 'median':
            broken_cohorts[0]['first_median_ns'] = str(float(broken_cohorts[0]['first_median_ns']) * 2)
        elif mutation == 'ratio':
            broken_cohorts[0]['second_over_first'] = '-1'
        else:
            row = next(row for row in broken_data[SEEDS[0]] if row['language'] == 'typescript')
            if mutation == 'sample':
                row['encode']['samples_ns'].pop()
            else:
                row['encode']['order_positions'] = [0] * 10
        try:
            verify(broken_data, broken_cohorts)
        except ValueError:
            continue
        raise ValueError(f'Cohort verifier accepted {mutation} mutation')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    datasets = {}
    for seed in SEEDS:
        directory = args.directory / f'seed-{seed}'
        datasets[seed] = json.loads((directory / 'samples.json').read_text())
        raw = [json.loads(line) for name in ('typescript.jsonl', 'csharp.jsonl', 'godot.jsonl')
               for line in (directory / name).read_text().splitlines() if line.startswith('{')]
        require(raw == datasets[seed], f'Raw process streams differ from aggregate: {seed}')
    with (args.directory / 'order-cohorts.csv').open() as file:
        cohorts = list(csv.DictReader(file))
    calculated = verify(datasets, cohorts)
    for language, codecs in PAIRED_CODECS.items():
        for codec in codecs:
            for operation in OPERATIONS:
                ratios = [value[2] for key, value in calculated.items()
                          if key[1:3] == (language, codec) and key[4] == operation]
                print(f'{language}/{codec}/{operation}: {min(ratios):.3f}–{max(ratios):.3f}')
    if args.self_test:
        self_test(datasets, cohorts)
    print(f'Passed {len(calculated)} cohort comparisons and raw-stream equality.' +
          (' Six negative controls pass.' if args.self_test else ''))


if __name__ == '__main__':
    main()
