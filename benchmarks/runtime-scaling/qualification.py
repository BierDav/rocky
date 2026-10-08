#!/usr/bin/env python3
"""Capture a trusted storage slice and report evidence without passing phase 8."""
import argparse
from datetime import datetime
import hashlib
import json
import platform
import re
import subprocess
import time
from pathlib import Path


def command(*args):
    try:
        result = subprocess.run(args, text=True, capture_output=True, check=False)
    except OSError as error:
        return {"exit_code": None, "stderr": str(error)}
    return {"exit_code": result.returncode, "stdout": result.stdout, "stderr": result.stderr}


def run(args):
    directory = args.directory.resolve()
    directory.mkdir(parents=True, exist_ok=False)
    binary = args.binary.resolve()
    probe = [str(binary), "soak", "--directory", str(directory / "data"),
             "--seconds", str(args.seconds), "--operations", str(args.operations),
             "--rows", str(args.rows), "--tables", str(args.tables),
             "--observers", str(args.observers), "--interval", str(args.interval),
             "--durability", args.durability]
    evidence = {"profile": args.profile, "platform": platform.platform(),
                "source_archive_sha256": args.source_sha,
                "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "rustc": command("rustc", "--version"), "command": probe,
                "started_unix": time.time(), "qualification_gate": "pending"}
    if platform.system() == "Linux":
        evidence["cpu"] = command("lscpu")
        evidence["storage"] = command("lsblk", "-o", "NAME,TYPE,SIZE,MODEL,FSTYPE")
    elif platform.system() == "Darwin":
        evidence["cpu"] = command("sysctl", "hw.model", "hw.memsize", "machdep.cpu.brand_string")
    evidence["filesystem"] = command("df", "-h", str(directory))
    manifest = directory / "profile.json"
    manifest.write_text(json.dumps(evidence, indent=2) + "\n")
    with (directory / "soak.txt").open("w") as output:
        result = subprocess.run(probe, stdout=output, stderr=subprocess.STDOUT, check=False)
    evidence.update(exit_code=result.returncode, finished_unix=time.time())
    if result.returncode:
        audit = [str(binary), "soak-audit", "--directory", str(directory / "data"),
                 "--tables", str(args.tables), "--durability", args.durability]
        with (directory / "audit.txt").open("w") as output:
            recovery = subprocess.run(audit, stdout=output, stderr=subprocess.STDOUT, check=False)
        evidence.update(audit_command=audit, audit_exit_code=recovery.returncode)
    manifest.write_text(json.dumps(evidence, indent=2) + "\n")
    print(f"{args.profile}: storage slice exit={result.returncode}; phase 8 pending; {directory}")
    return result.returncode


def number(text, key):
    matches = re.findall(rf"\b{re.escape(key)}=(\d+)", text)
    return max(map(int, matches)) if matches else None


def report(args):
    print("| Profile | Tables/domain | Live rows | Main operations | Observers | Peak RSS MiB | Reopen ms | Slice |")
    print("| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |")
    for directory in args.evidence:
        profile = json.loads((directory / "profile.json").read_text())
        text = (directory / "soak.txt").read_text()
        header = text.splitlines()[0] if text else ""
        audits = re.findall(r"soak audit domain=1 .*?live=(\d+) operations=(\d+)", text)
        live, operations = audits[-1] if audits else ("—", "—")
        passed = profile.get("exit_code") == 0 and "soak storage_slice=passed" in text and len(re.findall(r"soak audit domain=", text)) == 3
        label = profile["profile"].replace("|", "\\|").replace("\n", " ")
        values = [label, number(header, "tables"), live, operations, number(header, "observers"),
                  number(text, "resident_mib"), number(text, "reopen_ms"), "passed" if passed else "incomplete/failed"]
        print("| " + " | ".join("—" if value is None else str(value) for value in values) + " |")
    print("\nPhase 8 gate: pending. These are observed dimensions, not maximum supported limits.")
    print("\n| Budget or slice | Evidence still required |")
    print("| --- | --- |")
    for budget, requirement in [
        ("2,000 tables; 10 s install", "Single-domain install separated from the three-domain install"),
        ("5,000,000 live rows", "The soak defaults to 500,000; a distinct target-row run is needed"),
        ("Unbounded lifetime; bounded history", "Several hours, >=5,000,000 operations and plateau analysis on each profile"),
        ("Write cost within 2x", "Matched empty/full workload, plus CPU attribution of growing step cost"),
        ("Point read median <1 ms", "Full-domain measurements; smoke read latencies do not qualify"),
        ("Observer cost", "Changed-row and observer-count comparison at target size"),
        ("Open <2 s", "Target-size reopen at intervals, not just final smoke reopen"),
        ("RSS <256 MiB", "Target-size process RSS plateau independent of history"),
        ("10,000-operation catch-up", "Concurrent native topology with three voters and two partial replicas"),
        ("5,000 writes/s each interval; profile p99", "Ten-minute phase 5 gate on each hardware profile"),
        ("Fault correctness", "Kill/disconnect/queue/checkpoint/prune/rebootstrap in the combined topology"),
        ("Strong commands and effects", "Run throughout the same workload; existing separate probes are insufficient"),
        ("Encrypted scale", "Same workload with real grants/keys; trusted measurements cannot qualify it"),
        ("Durability", "Power-loss/fault-device evidence for both classes; process death is insufficient"),
    ]:
        print(f"| {budget} | {requirement} |")
    return 0


def record_probe(args):
    """Record a completed probe without rerunning its workload."""
    directory = args.directory.resolve()
    directory.mkdir(parents=True, exist_ok=True)
    binary = args.binary.resolve()
    started = datetime.fromisoformat(args.started_at.replace("Z", "+00:00")).timestamp()
    evidence = {
        "profile": args.profile,
        "platform": platform.platform(),
        "source_archive_sha256": args.source_sha,
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "rustc": command("rustc", "--version"),
        "command": args.command,
        "raw_output": str(args.raw.resolve()),
        "started_unix": started,
        "finished_unix": time.time(),
        "exit_code": args.exit_code,
        "qualification_gate": "phase 5 recheck; phase 8 pending",
    }
    if platform.system() == "Darwin":
        evidence["hardware"] = command("sysctl", "hw.model", "hw.memsize", "machdep.cpu.brand_string")
    evidence["filesystem"] = command("df", "-h", str(directory))
    (directory / "profile.json").write_text(json.dumps(evidence, indent=2) + "\n")
    return args.exit_code


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action", required=True)
    capture = commands.add_parser("run")
    capture.add_argument("--profile", required=True)
    capture.add_argument("--directory", type=Path, required=True)
    capture.add_argument("--binary", type=Path, default=Path("target/release/scaling_probe"))
    capture.add_argument("--source-sha", required=True, help="SHA256 of the deployed source archive")
    capture.add_argument("--seconds", type=int, default=10_800)
    capture.add_argument("--operations", type=int, default=5_000_000)
    capture.add_argument("--rows", type=int, default=500_000)
    capture.add_argument("--tables", type=int, default=2000)
    capture.add_argument("--observers", type=int, default=100)
    capture.add_argument("--interval", type=int, default=60)
    capture.add_argument("--durability", choices=["full", "buffered"], default="full")
    capture.set_defaults(handler=run)
    summary = commands.add_parser("report")
    summary.add_argument("evidence", type=Path, nargs="+")
    summary.set_defaults(handler=report)
    record = commands.add_parser("record-probe")
    record.add_argument("--profile", required=True)
    record.add_argument("--directory", type=Path, required=True)
    record.add_argument("--binary", type=Path, required=True)
    record.add_argument("--source-sha", required=True)
    record.add_argument("--raw", type=Path, required=True)
    record.add_argument("--started-at", required=True)
    record.add_argument("--exit-code", type=int, required=True)
    record.add_argument(
        "--command",
        nargs=argparse.REMAINDER,
        required=True,
        help="probe command, including any flags; must be the last option",
    )
    record.set_defaults(handler=record_probe)
    args = parser.parse_args()
    return args.handler(args)


if __name__ == "__main__":
    raise SystemExit(main())
