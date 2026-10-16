# Progressive checkpoint import on Android

Measured on 2026-10-13 with the [progressive importer](checkpoint-import.md).
This is one device run, not a latency or throughput qualification gate.
The run used the former aggregate readiness API; the current
[status contract](../proposals/observable-status.md) exposes import progress
independently and has no `Ready` verdict.

## Setup

A freshly reset Pixel 11 Pro joined a desktop JVM shopping household as Trusted
over LTE. The household contained four lists, 10,000 items in "Big", and 30
items in each other list. The desktop resumed a copy of the existing fixture;
its obsolete self-bootstrap intent was saved separately before opening it.
The original fixture was preserved.

The SDK was rebuilt and published to Maven local, extracted Android AAR caches
were cleared, and a debug APK was built and installed. Android used the SDK's
release native library; the desktop used its debug native library. Temporary
application logs recorded open phases, Room flow emissions and readiness. The
source instrumentation was removed after measurement. Raw logs and test-only
credentials remain under ignored `local/perf/`.

The phone's default network was LTE; its NetBird VPN was also active. Rocky's
peer diagnostics reported an open iroh link with 48 ms RTT. Rust checks and SDK
builds ran concurrently on the desktop, so these timings do not isolate radio,
source CPU, storage or UI costs.

## Fresh join

Times below use the application's `open start` log immediately after Join:

- Rocky opened at 5.015 s.
- The first list was observed at 12.767 s, 7.752 s after Rocky opened.
- All four lists were observed at 14.701 s.
- The initially selected small list showed its first items at 18.271 s.
- After selecting Big, its first 45 items were observed at 22.458 s.
- All 10,000 Big items were observed at 48.934 s.
- Readiness reached Ready at 49.759 s.

Readiness provided 250 progress samples with a fixed denominator of 10,112
stored rows across 18 scopes. Imported-row counts were monotonic and reached
10,112; the UI showed intermediate percentages, including 92%, before changing
to Live. The lists and selected list remained usable while verified pages
continued to arrive.

There was no `link channel read timeout` or behind-pruned-prefix failure. Initial
key delivery did encounter `connection lost`, and subsequently recovered. The
issuer's logs mislabeled one link failure as `local storage I/O`; this does not
establish a storage failure. A previously discovered, absent peer also produced
dial timeouts. The Pixel link remained open through the bulk import.

## Live edits

After bootstrap, five desktop commits to the selected small list reached its
Pixel Room flow in 157, 416, 135, 162 and 182 ms: median 162 ms, range
135–416 ms. These measure commit to observation, not frame presentation.
Clock correction used the best of 20 USB clock exchanges, with 46.7 ms
round-trip time, giving approximately ±23.4 ms clock uncertainty.

## Issuer regression and remaining limits

Sharing an issuer's own list back from another issuer previously created a
checkpoint-first intent despite the full replica already holding the partition.
The SDK now recognizes implicit holdings of known partitions on full replicas.
First keys still bootstrap new partitions; partial replicas still need explicit
holdings. The regression test reproduces the original failure and checks that
sharing the list back creates neither a bootstrap event nor an intent.
The measured desktop issuer stayed Ready after the Pixel join.

This run demonstrates completion where the earlier Pixel run stalled, and
observable verified pages and progress. It does **not** meet the target of first
data within a few seconds of Rocky opening. The measured build imported empty
scopes before populated scopes, and both scope work and page requests retained network
round-trip costs. No isolated profile or repeated idle-host run attributes the
remaining delay. Android network-change handling, relay-only joins and the
initial key-delivery connection loss remain unqualified here.

## Populated scopes first

The SDK now imports populated scopes smallest first, followed by empty scopes.
The interrupted-join regression checks that rows are readable before the empty
scope completes, survive reopening, and remain after the remaining scope resumes.
Its new visibility assertion fails with the previous order and passes with the
new one. Empty scopes still import and finish normally.

A separate JVM household on loopback contained the same four-list, 10,000-item
shape. Its checkpoints were warmed before comparison. Joiners used the previous
and updated native libraries, confirmed by their distinct packaged library hashes;
the previous library was explicitly selected for the repeated baseline. The
source and its other peers remained online for this comparison. Timings start
at the join request:

- Previous order: join returned 3.912 s; first list 5.636 s; all lists 5.848 s;
  first sampled Big item 6.979 s; Ready 14.415 s.
- Updated order: join returned 3.918 s; first list 4.755 s; all lists 5.031 s;
  first sampled Big item 6.031 s; Ready 12.278 s.

First-list observation after join returned was 1.724 s versus 0.837 s. Direct
database reads confirmed all 10,000 Big items on both joiners. Big-item timing
sampled five titles spread across the list; it does not time the first page or
the last row. An earlier baseline encountered an offline warm-up peer and was
excluded from this comparison.

This is one local comparison with debug native libraries, not a repeated latency
qualification. The active Pixel household was preserved, and the updated order
has not been remeasured on LTE. The original Pixel first-data limit therefore
remains open. Full Rust checks passed (163 unit, 941 integration), as did lint,
the shopping app's 39 tests and builds, and its iOS simulator test and app build.
