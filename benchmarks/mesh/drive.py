#!/usr/bin/env python3
"""Runs the multi-host Rocky scenarios; see README.md."""
import argparse
import itertools
import json
import os
import statistics
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mesh import Agent, AgentError, now_us  # noqa: E402

REMOTE_BIN = 'rocky-mesh-src/benchmarks/mesh/target/release/rocky-mesh-bench'
LOCAL_BIN = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'target/release/rocky-mesh-bench')
HOSTS = {
    'mb': {'ssh': None, 'lan': '192.168.0.186', 'data': os.path.expanduser('~/rocky-mesh-data')},
    'mm': {'ssh': 'david-mm-linz', 'lan': '192.168.0.220', 'data': '/Users/david-bieregger/rocky-mesh-data'},
    'tp': {'ssh': 'david-tp', 'lan': '192.168.0.71', 'data': '/home/david-bieregger/rocky-mesh-data'},
    'srv': {'ssh': 'david-server', 'lan': '192.168.6.198', 'data': '/home/david-bieregger/rocky-mesh-data'},
}
BODY = 256
BATCH = 100
ROWS_PER_PARTITION = 1250


class Run:
    def __init__(self, out):
        self.out = out
        os.makedirs(out, exist_ok=True)
        self.agents = {}
        self.seq = itertools.count(int(time.time()) * 1000)
        self.lock = threading.Lock()

    def record(self, scenario, **fields):
        fields = {'scenario': scenario, 'at_us': now_us(), **fields}
        with self.lock, open(os.path.join(self.out, f'{scenario}.jsonl'), 'a') as file:
            file.write(json.dumps(fields) + '\n')
        return fields

    def agent(self, name):
        if name not in self.agents:
            host = HOSTS[host_of(name)]
            if host['ssh'] is None:
                command = [LOCAL_BIN, 'agent']
            else:
                command = ['ssh', '-T', '-o', 'ServerAliveInterval=15', host['ssh'], REMOTE_BIN + ' agent']
            self.agents[name] = Agent(name, command, self.out)
        return self.agents[name]

    def sync_clocks(self, names):
        return {name: self.agent(name).measure_offset() for name in names}

    def stop(self):
        for agent in self.agents.values():
            try:
                agent.call('close', timeout=120)
            except AgentError:
                pass
            agent.stop()


def host_of(name):
    return name.split('@')[0]


def path_for(name, mode):
    if mode == 'p2p':
        return f'bind:{HOSTS[host_of(name)]["lan"]}:0'
    return mode


def directory(name, db):
    return f'{HOSTS[host_of(name)]["data"]}/{db}'


def wipe(names, db):
    for name in names:
        host = HOSTS[host_of(name)]
        target = directory(name, db)
        command = f'rm -rf {target} && mkdir -p {host["data"]}'
        if host['ssh'] is None:
            subprocess.run(command, shell=True, check=True)
        else:
            subprocess.run(['ssh', host['ssh'], command], check=True)


def introduce(run, names):
    contacts = {name: run.agent(name).call('contact')['contact'] for name in names}
    for left, right in itertools.permutations(names, 2):
        run.agent(left).call('add_peer', contact=contacts[right])


def form_mesh(run, creator, members, mode, db, rows=0, migrating=False):
    names = [creator] + members
    wipe(names, db)
    layout = 'partitioned_by_group' if migrating else 'flat'
    created = run.agent(creator).call('create', dir=directory(creator, db), path=path_for(creator, mode),
                                      layout=layout)
    groups = -(-rows // ROWS_PER_PARTITION) if migrating else 0
    if migrating:
        run.agent(creator).call('genesis_minimum', minimum=1)
    if rows and not migrating:
        run.agent(creator).call('seed', rows=rows, body=BODY, batch=BATCH, grp='base', timeout=3600)
    nodes = []
    for member in members:
        invitation = run.agent(creator).call('invite', issuer=False)['invitation']
        joined = run.agent(member).call('join', dir=directory(member, db), path=path_for(member, mode),
                                        invitation=invitation, layout=layout, timeout=1800)
        nodes.append(joined['node'])
        run.record('setup', db=db, member=member, join_ms=joined['join_ms'])
    introduce(run, names)
    if migrating:
        shared = run.agent(creator).call('share', groups=[f'base-{index}' for index in range(groups)],
                                         nodes=nodes, timeout=1800)
        run.record('setup', db=db, shared=shared)
        seeded = run.agent(creator).call('seed', rows=rows, body=BODY, batch=BATCH, grp='base', groups=groups,
                                         timeout=3600)
        run.record('setup', db=db, seeded=seeded)
    if rows:
        for member in members:
            run.agent(member).call('wait', sql='SELECT count(*) FROM items', at_least=rows,
                                   timeout_ms=1800_000, timeout=1900)
    return created


def reopen(run, names, mode, db, version):
    for name in names:
        run.agent(name).call('close', timeout=300)
    calls = {name: run.agent(name).start('open', dir=directory(name, db), path=path_for(name, mode),
                                         version=version) for name in names}
    opened = {name: call.result(3600) for name, call in calls.items()}
    introduce(run, names)
    return opened


def probe_once(run, scenario, origin, receivers, label, timeout=30):
    seq = next(run.seq)
    sender = run.agent(origin)
    probe = sender.call('probe', seq=seq)
    start = sender.to_driver(probe['start_us'])
    deadline = time.monotonic() + timeout
    results = []
    for receiver in receivers:
        seen = run.agent(receiver).wait_seen(seq, deadline)
        arrival = None if seen is None else run.agent(receiver).to_driver(seen['at_us'])
        results.append(run.record(scenario, kind='probe', label=label, seq=seq, origin=origin,
                                  receiver=receiver, commit_ms=probe['commit_ms'],
                                  one_way_ms=None if arrival is None else (arrival - start) / 1000))
    return results


def latency_rounds(run, scenario, names, rounds, label, gap=0.25):
    run.sync_clocks(names)
    for _ in range(rounds):
        for origin in names:
            probe_once(run, scenario, origin, [name for name in names if name != origin], label)
            time.sleep(gap)
    run.sync_clocks(names)


class Prober(threading.Thread):
    def __init__(self, run, scenario, pairs, label, period=0.5):
        super().__init__(daemon=True)
        self.run_, self.scenario, self.pairs, self.label, self.period = run, scenario, pairs, label, period
        self.halt = threading.Event()

    def run(self):
        turn = 0
        while not self.halt.is_set():
            pairs = self.pairs
            if not pairs:
                self.halt.wait(self.period)
                continue
            origin, receivers = pairs[turn % len(pairs)]
            turn += 1
            try:
                probe_once(self.run_, self.scenario, origin, receivers, self.label(), timeout=60)
            except AgentError as error:
                self.run_.record(self.scenario, kind='probe_error', label=self.label(), error=str(error))
            self.halt.wait(self.period)

    def stop(self):
        self.halt.set()
        self.join(120)


def traffic_delta(after, before):
    return {key: after[key] - before.get(key, 0) for key in after}


def scenario_latency(run, args):
    names = ['mm', 'mb', 'tp', 'srv']
    for mode in args.modes:
        db = f'latency-{mode}'
        form_mesh(run, 'mm', ['mb', 'tp', 'srv'], mode, db)
        time.sleep(5)
        latency_rounds(run, 'latency', names, args.rounds, mode)
        for name in names:
            peers = {other: run.agent(other).call('address')['address'] for other in names if other != name}
            for other, address in peers.items():
                run.record('latency', kind='paths', label=mode, node=name, peer=other,
                           paths=run.agent(name).call('paths', address=address)['paths'])
        for name in names:
            run.agent(name).call('close')
        wipe(names, db)


def scenario_loopback(run, args):
    wipe(['mb'], 'loop-a')
    left = run.agent('mb')
    right_name = 'mb2'
    HOSTS[right_name] = dict(HOSTS['mb'])
    wipe([right_name], 'loop-b')
    left.call('create', dir=directory('mb', 'loop-a'), path='bind:127.0.0.1:0')
    invitation = left.call('invite', issuer=False)['invitation']
    right = run.agent(right_name)
    right.call('join', dir=directory(right_name, 'loop-b'), path='bind:127.0.0.1:0', invitation=invitation)
    introduce(run, ['mb', right_name])
    time.sleep(3)
    latency_rounds(run, 'loopback', ['mb', right_name], args.rounds, 'loopback')
    left.call('close')
    right.call('close')


def scenario_bootstrap(run, args):
    pairs = [('srv', 'mm'), ('srv', 'tp'), ('srv', 'mb'), ('mm', 'tp'), ('mm', 'mb')]
    sources = sorted({source for source, _ in pairs})
    for mode in args.modes:
        for source in sources:
            db = f'boot-{source}'
            src = run.agent(source)
            if mode == args.modes[0]:
                wipe([source], db)
                src.call('create', dir=directory(source, db), path=path_for(source, mode))
                seeded = src.call('seed', rows=args.rows, body=BODY, batch=BATCH, grp='base', timeout=3600)
                run.record('bootstrap', kind='seed', source=source, **seeded)
            else:
                src.call('close')
                src.call('open', dir=directory(source, db), path=path_for(source, mode), version=1)
            for target in [t for s, t in pairs if s == source]:
                tdb = f'boot-{source}-{target}'
                wipe([target], tdb)
                dst = run.agent(f'{target}@target')
                invitation = src.call('invite', issuer=False)['invitation']
                before_src = src.call('traffic')
                began = time.monotonic()
                joined = dst.call('join', dir=directory(target, tdb), path=path_for(target, mode),
                                  invitation=invitation, timeout=3600)
                waited = dst.call('wait', sql='SELECT count(*) FROM items', at_least=args.rows,
                                  timeout_ms=1800_000, timeout=1900)
                total = time.monotonic() - began
                received = dst.call('traffic')
                sent = traffic_delta(src.call('traffic'), before_src)
                paths = dst.call('paths', address=src.call('address')['address'])['paths']
                stats = dst.call('stats')
                run.record('bootstrap', kind='transfer', mode=mode, source=source, target=target,
                           rows=args.rows, join_ms=joined['join_ms'], wait_ms=waited['ms'],
                           reached=waited['reached'], total_s=total, received=received, source_sent=sent,
                           paths=paths, samples=waited['samples'], target_stats=stats)
                dst.call('close')
                wipe([target], tdb)
        if mode == args.modes[-1]:
            for source in sources:
                run.agent(source).call('close')


def all_pairs(names):
    return [(origin, [receiver for receiver in names if receiver != origin]) for origin in names]


def scenario_migration(run, args):
    db = 'migration'
    layout = 'partitioned_by_group'
    tagged = "SELECT count(*) FROM items WHERE tag != ''"
    members = ['mb', 'tp', 'srv']
    names = ['mm'] + members
    form_mesh(run, 'mm', members, args.mode, db, rows=args.rows, migrating=True)
    run.sync_clocks(names)
    issuer = run.agent('mm')
    phase = {'name': 'before'}
    prober = Prober(run, 'migration', all_pairs(names), lambda: phase['name'])
    prober.start()
    time.sleep(15)
    phase['name'] = 'issuer_migrating'
    prober.pairs = all_pairs(members)
    issuer.call('close')
    opened = issuer.call('open', dir=directory('mm', db), path=path_for('mm', args.mode), version=2,
                         layout=layout, timeout=3600)
    run.record('migration', kind='issuer_open', open_ms=opened['open_ms'],
               tagged=issuer.call('scalar', sql=tagged)['value'], stats=issuer.call('stats'))
    introduce(run, names)
    phase['name'] = 'issuer_migrated'
    prober.pairs = all_pairs(names)
    before = {member: run.agent(member).call('traffic') for member in members}
    receiving = {member: run.agent(member).start('wait', sql=tagged, at_least=args.rows, timeout_ms=900_000)
                 for member in members}
    for member, call in receiving.items():
        waited = call.result(1000)
        run.record('migration', kind='member_received', member=member, wait_ms=waited['ms'],
                   reached=waited['reached'], first_answer_ms=waited.get('first_answer_ms'),
                   traffic=traffic_delta(run.agent(member).call('traffic'), before[member]),
                   samples=waited['samples'])
    time.sleep(10)
    for member in members:
        phase['name'] = f'upgrading_{member}'
        prober.pairs = all_pairs([name for name in names if name != member])
        agent = run.agent(member)
        agent.call('close')
        began = time.monotonic()
        opened = agent.call('open', dir=directory(member, db), path=path_for(member, args.mode), version=2,
                            layout=layout, timeout=3600)
        introduce(run, names)
        prober.pairs = all_pairs(names)
        waited = agent.call('wait', sql=tagged, at_least=args.rows, timeout_ms=1800_000, timeout=1900)
        run.record('migration', kind='member_upgrade', member=member, open_ms=opened['open_ms'],
                   wait_ms=waited['ms'], reached=waited['reached'], first_answer_ms=waited.get('first_answer_ms'),
                   total_s=time.monotonic() - began, traffic=agent.call('traffic'), samples=waited['samples'],
                   stats=agent.call('stats'))
    phase['name'] = 'after'
    prober.pairs = all_pairs(names)
    time.sleep(20)
    prober.stop()
    for name in names:
        run.record('migration', kind='final', node=name, stats=run.agent(name).call('stats'),
                   traffic=run.agent(name).call('traffic'))


def scenario_offline(run, args):
    db = 'offline'
    members = ['mb', 'tp', 'srv']
    names = ['mm'] + members
    form_mesh(run, 'mm', members, args.mode, db, rows=args.rows)
    run.sync_clocks(names)
    away = run.agent(args.away)
    others = [name for name in names if name != args.away]
    phase = {'name': 'baseline'}
    prober = Prober(run, 'offline', [(origin, [r for r in others if r != origin]) for origin in others],
                    lambda: phase['name'])
    prober.start()
    time.sleep(10)
    for tier, changes in enumerate(args.tiers):
        phase['name'] = f'tier{tier}_offline'
        away.call('networking', enabled=False)
        offline_grp, online_grp = f'off-{tier}', f'on-{tier}'
        offline_write = away.start('seed', rows=changes, body=BODY, batch=BATCH, grp=offline_grp)
        online_write = run.agent('mm').start('seed', rows=args.online, body=BODY, batch=BATCH, grp=online_grp)
        written_offline = offline_write.result(3600)
        touched = away.call('touch', rows=min(changes // 2, args.rows), batch=BATCH, grp='base', timeout=3600)
        written_online = online_write.result(3600)
        seals = [run.agent('mm').call('seal', timeout=600)['ms'] for _ in range(args.seals)]
        run.record('offline', kind='offline_writes', tier=tier, changes=changes, offline=written_offline,
                   touched=touched, online=written_online, seals_ms=seals)
        before = {name: run.agent(name).call('traffic') for name in names}
        phase['name'] = f'tier{tier}_catchup'
        began = time.monotonic()
        away.call('networking', enabled=True)
        waits = {name: run.agent(name).start(
            'wait', sql=f"SELECT count(*) FROM items WHERE grp='{offline_grp}'", at_least=changes,
            timeout_ms=1800_000) for name in others}
        waits[args.away] = away.start('wait', sql=f"SELECT count(*) FROM items WHERE grp='{online_grp}'",
                                      at_least=args.online, timeout_ms=1800_000)
        for name, call in waits.items():
            waited = call.result(1900)
            run.record('offline', kind='converged', tier=tier, changes=changes, node=name, wait_ms=waited['ms'],
                       reached=waited['reached'], samples=waited['samples'],
                       traffic=traffic_delta(run.agent(name).call('traffic'), before[name]))
        run.record('offline', kind='tier_done', tier=tier, total_s=time.monotonic() - began)
        phase['name'] = f'tier{tier}_after'
        time.sleep(10)
    prober.stop()


def scenario_local(run, args):
    for name in args.hosts:
        host = HOSTS[name]
        target = f'{host["data"]}/local'
        command = [LOCAL_BIN] if host['ssh'] is None else ['ssh', host['ssh'], REMOTE_BIN]
        argv = ['local', target, str(args.single), str(BATCH), str(args.batches), str(BODY)]
        if host['ssh'] is None:
            output = subprocess.run(command + argv, capture_output=True, text=True, check=True).stdout
        else:
            output = subprocess.run(command + [' '.join(argv)], capture_output=True, text=True, check=True).stdout
        for line in output.splitlines():
            run.record('local', host=name, **json.loads(line))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('scenario', choices=['latency', 'loopback', 'bootstrap', 'migration', 'offline', 'local'])
    parser.add_argument('--out', required=True)
    parser.add_argument('--modes', nargs='+', default=['p2p', 'relay'])
    parser.add_argument('--mode', default='p2p')
    parser.add_argument('--rounds', type=int, default=20)
    parser.add_argument('--rows', type=int, default=50_000)
    parser.add_argument('--tiers', type=int, nargs='+', default=[1_000, 10_000, 50_000])
    parser.add_argument('--online', type=int, default=2_000)
    parser.add_argument('--seals', type=int, default=3)
    parser.add_argument('--away', default='tp')
    parser.add_argument('--hosts', nargs='+', default=list(HOSTS))
    parser.add_argument('--single', type=int, default=1000)
    parser.add_argument('--local', action='store_true',
                        help='run every node as a process on this machine, over loopback')
    parser.add_argument('--batches', type=int, default=50)
    args = parser.parse_args()
    if args.local:
        for name in list(HOSTS):
            HOSTS[name] = {'ssh': None, 'lan': '127.0.0.1',
                           'data': os.path.expanduser(f'~/rocky-mesh-data/local-{name}')}
    run = Run(args.out)
    run.record('meta', scenario_name=args.scenario, args=vars(args))
    try:
        globals()[f'scenario_{args.scenario}'](run, args)
    finally:
        run.stop()


if __name__ == '__main__':
    main()
