#!/usr/bin/env python3
"""Turns a results directory from drive.py into Markdown tables on standard output."""
import collections
import json
import os
import sys

MB = 1_000_000


def load(directory, scenario):
    path = os.path.join(directory, f'{scenario}.jsonl')
    if not os.path.exists(path):
        return []
    with open(path) as file:
        return [json.loads(line) for line in file if line.strip()]


def quantile(values, q):
    values = sorted(values)
    if not values:
        return float('nan')
    return values[round((len(values) - 1) * q)]


def table(header, rows):
    print('| ' + ' | '.join(header) + ' |')
    print('| ' + ' | '.join(['---'] + ['---:'] * (len(header) - 1)) + ' |')
    for row in rows:
        print('| ' + ' | '.join(str(cell) for cell in row) + ' |')
    print()


def f(value, digits=1):
    if value is None or value != value:
        return '–'
    return f'{value:,.{digits}f}'


def local(directory):
    lines = [line for line in load(directory, 'local') if 'variant' in line]
    if not lines:
        return
    print('## Local commit cost: plain SQLite vs Rocky\n')
    rows = []
    for line in lines:
        phases = line.get('phases') or {}
        rows.append([line['host'], line['variant'], f(line['insert_us']['p50'], 0), f(line['insert_us']['p99'], 0),
                     f(line['update_us']['p50'], 0), f(line['read_us']['p50'], 0),
                     f(line['batch_rows_per_s'], 0), f(line['disk_bytes_per_row'], 0),
                     f(phases.get('commit_us', {}).get('p50'), 0)])
    table(['host', 'variant', 'insert p50 µs', 'insert p99 µs', 'update p50 µs', 'point read p50 µs',
           'batched rows/s', 'disk bytes/row', 'commit() p50 µs'], rows)


def probes(lines):
    return [line for line in lines if line.get('kind') == 'probe']


def latency_summary(values):
    return [f(quantile(values, 0.5)), f(quantile(values, 0.9)), f(quantile(values, 0.99)), f(max(values) if values else None)]


def loopback(directory):
    lines = probes(load(directory, 'loopback'))
    if not lines:
        return
    print('## Loopback: two processes on david-mb\n')
    values = [line['one_way_ms'] for line in lines if line['one_way_ms'] is not None]
    commits = [line['commit_ms'] for line in lines]
    table(['samples', 'one-way p50 ms', 'p90', 'p99', 'max', 'commit p50 ms'],
          [[len(values)] + latency_summary(values) + [f(quantile(commits, 0.5))]])


def latency(directory):
    lines = load(directory, 'latency')
    samples = probes(lines)
    if not samples:
        return
    print('## Small-change latency in a four-node mesh\n')
    paths = {}
    for line in lines:
        if line.get('kind') == 'paths':
            selected = [path for path in line['paths'] if path['selected']]
            if selected:
                best = max(selected, key=lambda path: path['received'] + path['sent'])
                paths[(line['label'], line['node'], line['peer'])] = (best['kind'], best['rtt_us'] / 1000, best['remote'])
    grouped = collections.defaultdict(list)
    missing = collections.Counter()
    for line in samples:
        key = (line['label'], line['origin'], line['receiver'])
        if line['one_way_ms'] is None:
            missing[key] += 1
        else:
            grouped[key].append(line['one_way_ms'])
    rows = []
    for (mode, origin, receiver), values in sorted(grouped.items()):
        back = grouped.get((mode, receiver, origin), [])
        symmetric = (quantile(values, 0.5) + quantile(back, 0.5)) / 2 if back else None
        kind, rtt, _ = paths.get((mode, receiver, origin), ('?', None, ''))
        rows.append([mode, f'{origin} → {receiver}', len(values), *latency_summary(values), f(symmetric), kind,
                     f(rtt), missing[(mode, origin, receiver)]])
    table(['path', 'pair', 'n', 'p50 ms', 'p90', 'p99', 'max', 'pair-symmetric p50', 'selected path', 'path RTT ms',
           'lost'], rows)
    by_mode = collections.defaultdict(list)
    commits = collections.defaultdict(list)
    for line in samples:
        if line['one_way_ms'] is not None:
            by_mode[line['label']].append(line['one_way_ms'])
        commits[(line['label'], line['origin'])].append(line['commit_ms'])
    table(['path', 'all pairs n', 'p50 ms', 'p90', 'p99', 'max'],
          [[mode, len(values), *latency_summary(values)] for mode, values in sorted(by_mode.items())])
    table(['path', 'origin', 'commit p50 ms', 'commit p90 ms'],
          [[mode, origin, f(quantile(values, 0.5)), f(quantile(values, 0.9))]
           for (mode, origin), values in sorted(commits.items())])


def bootstrap(directory):
    lines = load(directory, 'bootstrap')
    transfers = [line for line in lines if line.get('kind') == 'transfer']
    if not transfers:
        return
    print('## Fresh bootstrap\n')
    for line in lines:
        if line.get('kind') == 'seed':
            print(f"- seed on {line['source']}: {line['rows']:,} rows in {line['ms'] / 1000:.1f} s "
                  f"({line['rows_per_s']:,.0f} rows/s)")
    print()
    rows = []
    for line in transfers:
        received = line['received']
        wire = received['direct_received'] + received['relay_received']
        seconds = line['total_s']
        selected = [path for path in line['paths'] if path['selected']]
        kind = '/'.join(sorted({path['kind'] for path in selected})) or '?'
        rtt = min((path['rtt_us'] for path in selected), default=None)
        rows.append([line['mode'], f"{line['source']} → {line['target']}", f"{line['rows']:,}",
                     f(line['join_ms'] / 1000, 2), f(seconds, 1), f(line['rows'] / seconds, 0),
                     f(wire / MB, 1), f(received['direct_received'] / MB, 1), f(received['relay_received'] / MB, 1),
                     f(wire / MB / seconds, 2), f(wire * 8 / MB / seconds, 1), kind,
                     f(None if rtt is None else rtt / 1000), '' if line['reached'] else 'timeout'])
    table(['path', 'pair', 'rows', 'join() s', 'total s', 'rows/s', 'received MB', 'direct MB', 'relay MB',
           'MB/s', 'Mbit/s', 'selected', 'RTT ms', ''], rows)


def phase_latency(lines, scenario):
    grouped = collections.defaultdict(list)
    lost = collections.Counter()
    errors = collections.Counter()
    order = []
    for line in lines:
        label = line.get('label')
        if label is None:
            continue
        if label not in order:
            order.append(label)
        if line.get('kind') == 'probe_error':
            errors[label] += 1
        elif line.get('kind') == 'probe':
            if line['one_way_ms'] is None:
                lost[label] += 1
            else:
                grouped[label].append(line['one_way_ms'])
    table(['phase', 'n', 'p50 ms', 'p90', 'p99', 'max', 'not seen in 60 s', 'probe errors'],
          [[label, len(grouped[label]), *latency_summary(grouped[label]), lost[label], errors[label]]
           for label in order])


def migration(directory):
    lines = load(directory, 'migration')
    if not lines:
        return
    print('## Migration of every row\n')
    setup = [line for line in load(directory, 'setup') if line.get('db') == 'migration']
    for line in lines:
        if line.get('kind') == 'issuer_open':
            print(f"- issuer (mm) open with v2: {line['open_ms'] / 1000:.1f} s, {line['tagged']:,} rows tagged, "
                  f"cpu {line['stats']['cpu_ms'] / 1000:.1f} s total, rss {line['stats']['rss_bytes'] / MB:.0f} MB")
    print()
    rows = []
    for line in lines:
        if line.get('kind') == 'member_upgrade':
            traffic = line['traffic']
            rows.append([line['member'], f(line['open_ms'] / 1000, 2), f(line['wait_ms'] / 1000, 1),
                         f(line['total_s'], 1),
                         f((traffic['direct_received'] + traffic['relay_received']) / MB, 1),
                         '' if line['reached'] else 'timeout'])
    table(['member', 'open v2 s', 'until all rows tagged s', 'total s', 'received MB (whole session)', ''], rows)
    rows = []
    for line in lines:
        if line.get('kind') == 'member_received':
            traffic = line['traffic']
            rows.append([line['member'], f(line['wait_ms'] / 1000, 1),
                         f((traffic['direct_received'] + traffic['relay_received']) / MB, 1),
                         f((traffic['direct_sent'] + traffic['relay_sent']) / MB, 1),
                         '' if line['reached'] else 'timeout'])
    if rows:
        print('Members still on the v1 application, from the issuer reopening on v2:\n')
        table(['member', 'all migrated rows present after s', 'received MB', 'sent MB', ''], rows)
    phase_latency(lines, 'migration')


def offline(directory):
    lines = load(directory, 'offline')
    if not lines:
        return
    print('## A peer returning with offline changes\n')
    rows = []
    for line in lines:
        if line.get('kind') == 'offline_writes':
            rows.append([line['tier'], f"{line['changes']:,}", f(line['offline']['ms'] / 1000, 1),
                         f(line['touched']['ms'] / 1000, 1), f"{line['online']['rows']:,}",
                         ', '.join(f(ms / 1000, 2) for ms in line['seals_ms'])])
    table(['tier', 'offline inserts', 'insert s', 'update half s', 'online inserts', 'seals s'], rows)
    rows = []
    for line in lines:
        if line.get('kind') == 'converged':
            traffic = line['traffic']
            rows.append([line['tier'], f"{line['changes']:,}", line['node'], f(line['wait_ms'] / 1000, 1),
                         f((traffic['direct_received'] + traffic['relay_received']) / MB, 1),
                         f((traffic['direct_sent'] + traffic['relay_sent']) / MB, 1),
                         '' if line['reached'] else 'timeout'])
    table(['tier', 'offline changes', 'node', 'converged after s', 'received MB', 'sent MB', ''], rows)
    phase_latency(lines, 'offline')


def main():
    directory = sys.argv[1]
    for section in (local, loopback, latency, bootstrap, migration, offline):
        section(directory)


if __name__ == '__main__':
    main()
