#!/usr/bin/env python3
"""Parse soak.txt interval lines into merged tables for the observer-growth arms.

Usage: analyze.py <arm-dir> [<arm-dir> ...]
"""
import re
import sys
from pathlib import Path


def fields(line):
    out = {}
    for key, value in re.findall(r"([a-z_]+)=([^\s]+)", line):
        out[key] = value
    return out


def parse(arm):
    text = (arm / "soak.txt").read_text()
    rows = {}
    order = []
    current = None
    for line in text.splitlines():
        if line.startswith("soak t_s="):
            current = fields(line)["t_s"]
            if current not in rows:
                rows[current] = {}
                order.append(current)
            rows[current].update(fields(line))
        elif line.startswith("soak subphases"):
            rows.setdefault(current, {}).update(fields(line))
        elif line.startswith("soak storage"):
            rows.setdefault(current, {}).update(fields(line))
        elif line.startswith("soak observer deliveries:"):
            rows.setdefault(current, {}).update(
                {f"obs_{k}": v for k, v in fields(line.split(":", 1)[1]).items()}
            )
        elif line.startswith("soak point reads:"):
            rows.setdefault(current, {}).update(
                {f"pt_{k}": v for k, v in fields(line.split(":", 1)[1]).items()}
            )
    return order, rows


def main():
    for name in sys.argv[1:]:
        arm = Path(name)
        order, rows = parse(arm)
        print(f"\n## {arm.name}")
        print(
            "| t | writes/s | step µs | admission | reduction | encoding | sql | "
            "obs/s | obs p50 | obs p99 | obs max | retained | db MB | wal MB | free% |"
        )
        print(
            "| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
        )
        for t in order:
            r = rows[t]

            def f(key, scale=1.0, nd=2):
                try:
                    return f"{float(r[key]) / scale:.{nd}f}"
                except (KeyError, ValueError):
                    return "—"

            print(
                "| "
                + " | ".join(
                    [
                        str(t),
                        f("writes_per_s", nd=0),
                        f("step_us_per_write"),
                        f("admission_us_per_write"),
                        f("reduction_us_per_write"),
                        f("encoding_us_per_write"),
                        f("sql_us_per_write"),
                        f("observer_deliveries_per_s", nd=0),
                        r.get("obs_p50_us<=", "—"),
                        r.get("obs_p99_us<=", "—"),
                        r.get("obs_max_us", "—"),
                        r.get("retained", "—"),
                        f("db_bytes", 1_000_000, 1),
                        f("wal_bytes", 1_000_000, 1),
                        f("free_pct"),
                    ]
                )
                + " |"
            )


if __name__ == "__main__":
    main()
