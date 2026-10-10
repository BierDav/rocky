"""Attributes a macOS `sample` call graph of one thread to named costs.

Usage: analyze.py <sample.txt> <thread substring> [wall seconds]

Each sample is assigned to the outermost frame on its stack that matches a
category, so signatures inside chain verification count as chain
verification. Prints samples, shares and, given the stage's wall time,
seconds per category, plus the heaviest unattributed leaf frames.
"""
import gzip
import re
import sys
from collections import Counter, defaultdict

CATEGORIES = [
    # (name, substrings of mangled or plain symbol names)
    ("wait (round trips, idle)", ["kevent", "__psynch_cvwait", "semaphore_wait", "semaphore_timedwait", "__select", "poll ", "mach_msg"]),
    ("flush (fsync)", ["fsync", "fcntl", "8sync_all"]),
    ("chain verification and credential resolution", ["17rocky_credentials", "12verify_chain", "13resolve_chain", "12local_grants", "14authorize_peer", "16verify_selectors"]),
    ("sealing (encrypt, decrypt)", ["chacha20", "poly1305", "4aead"]),
    ("signatures (frames, checkpoints)", ["ed25519", "curve25519"]),
    ("storage (SQLite)", ["sqlite3", "rusqlite"]),
    ("encoding (CBOR)", ["ciborium", "serde_cbor", "minicbor"]),
    ("hashing", ["blake3", "sha2", "6blake2"]),
]


def parse(path, thread):
    opener = gzip.open if str(path).endswith(".gz") else open
    with opener(path, "rt", encoding="utf-8", errors="replace") as source:
        lines = source.read().splitlines()
    start = lines.index("Call graph:") + 1
    nodes = []  # (depth, count, name, parent index)
    stack = []
    active = False
    for line in lines[start:]:
        if not line.strip():
            break
        match = re.match(r"^([ +!:|]*)(\d+) (.*)$", line)
        if not match:
            continue
        depth = len(match.group(1))
        count = int(match.group(2))
        name = match.group(3)
        if depth == 4:  # a thread
            active = thread in name
            stack = []
            if active:
                nodes.append((depth, count, name, None))
                stack = [len(nodes) - 1]
            continue
        if not active:
            continue
        while stack and nodes[stack[-1]][0] >= depth:
            stack.pop()
        nodes.append((depth, count, name, stack[-1] if stack else None))
        stack.append(len(nodes) - 1)
    return nodes


def pretty(name):
    """The identifiers of a v0-mangled Rust symbol, as a short path."""
    name = re.sub(r"\s+\(in .*$", "", name)
    if not name.startswith("_R"):
        return name[:80]
    idents, index = [], 0
    while index < len(name):
        match = re.match(r"(\d+)", name[index:])
        if match:
            length = int(match.group(1))
            start = index + len(match.group(1))
            if start < len(name) and name[start] == "_":
                start += 1
            ident = name[start:start + length]
            if length > 0 and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", ident):
                idents.append(ident)
                index = start + length
                continue
        index += 1
    keep = [i for i in idents if i not in ("core", "alloc", "std", "ops", "function", "future")]
    return "::".join(keep[-4:])


def category(name):
    for label, needles in CATEGORIES:
        if any(needle in name for needle in needles):
            return label
    return None


def main():
    path, thread = sys.argv[1], sys.argv[2]
    wall = float(sys.argv[3]) if len(sys.argv) > 3 else None
    nodes = parse(path, thread)
    children = defaultdict(int)
    for depth, count, name, parent in nodes:
        if parent is not None:
            children[parent] += count
    assigned = []
    totals = Counter()
    other = Counter()
    for index, (depth, count, name, parent) in enumerate(nodes):
        inherited = assigned[parent] if parent is not None else None
        own = inherited or category(name)
        assigned.append(own)
        own_samples = count - children[index]
        if own_samples <= 0:
            continue
        totals[own or "other"] += own_samples
        if own is None:
            short = pretty(name)
            other[short] += own_samples
    total = sum(totals.values())
    print(f"samples {total}" + (f" over {wall:.1f} s" if wall else ""))
    for label, samples in totals.most_common():
        seconds = f" {wall * samples / total:7.1f} s" if wall else ""
        print(f"  {100 * samples / total:5.1f}%{seconds}  {label}")
    if len(sys.argv) > 4:
        # Entry points of one category, by their nearest Rocky caller.
        wanted = sys.argv[4]
        entries = Counter()
        for index, (depth, count, name, parent) in enumerate(nodes):
            if assigned[index] is None or wanted not in assigned[index]:
                continue
            if parent is not None and assigned[parent] == assigned[index]:
                continue
            caller = parent
            path = []
            while caller is not None and len(path) < int(sys.argv[5] if len(sys.argv) > 5 else 2):
                frame = nodes[caller][2]
                if "rocky_" in frame and "tokio" not in frame:
                    path.append(pretty(frame))
                caller = nodes[caller][3]
            entries[" <- ".join(path)] += count
        print(f"{wanted} by caller:")
        for name, samples in entries.most_common(int(sys.argv[6]) if len(sys.argv) > 6 else 15):
            print(f"  {100 * samples / total:5.1f}%  {name}")
    print("heaviest unattributed frames:")
    for name, samples in other.most_common(12):
        print(f"  {100 * samples / total:5.1f}%  {name}")


if __name__ == "__main__":
    main()
