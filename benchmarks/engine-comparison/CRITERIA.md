# Decision criteria (defined before measurements)

Recommend Turso if correctness/durability gates pass and either:
* representative bottleneck workloads show >=25% sustained committed throughput benefit, with p99 no worse than 10%, including capture and consumption; or
* built-in capture demonstrably removes substantial reliable integration work (transaction grouping, rollback coverage, schema evolution, typed before/after), enough to outweigh measured overhead and operational/compatibility gaps.

A <10% speed difference is inconclusive. 10–25% is workload-specific evidence, not a universal engine decision. Compare one file separately from four partitions. Require repeated release measurements; report unsupported cases and failures, never count failed mutations as throughput. Crash tests establish process-crash recovery, not physical power-loss durability. Missing correctness gates imply insufficient evidence, irrespective of speed. CDC never supplies logical intent, signatures, encryption, policy or the canonical p2panda log.
