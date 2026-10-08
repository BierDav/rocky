# Measurements

Medians of repeated runs. Full ranges, CPU, memory, storage and consumer metrics are in summary.csv. Failed cases are excluded and listed below.

| Work / writers / batch / files | Adapter | runs | tx/s | mutations/s | p50 / p95 / p99 ms | retries/commit | total MiB |
|---|---|---:|---:|---:|---|---:|---:|
| draft / 1 / 1 / 1 | sqlite none | 3 | 249.4 | 249.4 | 3.98 / 4.09 / 7.82 | 0.00 | 0.17 |
| draft / 1 / 1 / 1 | sqlite trigger | 3 | 239.7 | 239.7 | 4.00 / 4.15 / 7.87 | 0.00 | 0.20 |
| draft / 1 / 1 / 1 | sqlite explicit | 3 | 250.7 | 250.7 | 3.98 / 4.87 / 6.61 | 0.00 | 0.17 |
| draft / 1 / 1 / 1 | wal none | 3 | 253.4 | 253.4 | 4.00 / 4.26 / 5.38 | 0.00 | 1.05 |
| draft / 1 / 1 / 1 | wal cdc | 3 | 254.7 | 254.7 | 3.99 / 4.09 / 4.92 | 0.00 | 1.52 |
| draft / 1 / 1 / 1 | wal explicit | 3 | 273.5 | 273.5 | 3.98 / 4.09 / 5.00 | 0.00 | 1.27 |
| draft / 1 / 1 / 1 | mvcc none | 3 | 246.7 | 246.7 | 4.00 / 4.52 / 5.07 | 0.00 | 0.12 |
| draft / 1 / 1 / 1 | mvcc cdc | 3 | 45.0 | 45.0 | 21.99 / 24.01 / 26.77 | 0.00 | 0.17 |
| draft / 1 / 1 / 1 | mvcc explicit | 3 | 301.4 | 301.4 | 3.05 / 4.05 / 5.05 | 0.00 | 0.13 |
| independent / 4 / 1 / 1 | sqlite none | 3 | 285.5 | 285.5 | 3.05 / 4.11 / 151.56 | 3.66 | 0.16 |
| independent / 4 / 1 / 1 | sqlite trigger | 3 | 255.5 | 255.5 | 3.98 / 4.20 / 149.99 | 4.54 | 0.22 |
| independent / 4 / 1 / 1 | sqlite explicit | 3 | 286.6 | 286.6 | 3.08 / 4.17 / 163.98 | 3.68 | 0.17 |
| independent / 4 / 1 / 1 | wal none | 3 | 286.2 | 286.2 | 3.12 / 5.00 / 161.64 | 4.03 | 1.60 |
| independent / 4 / 1 / 1 | wal cdc | 3 | 249.6 | 249.6 | 4.00 / 5.02 / 209.67 | 5.08 | 3.08 |
| independent / 4 / 1 / 1 | wal explicit | 3 | 269.7 | 269.7 | 3.97 / 4.94 / 187.81 | 4.76 | 2.30 |
| independent / 4 / 1 / 1 | mvcc none | 3 | 641.5 | 641.5 | 6.00 / 7.97 / 8.27 | 0.00 | 0.13 |
| independent / 4 / 1 / 1 | mvcc explicit | 3 | 630.6 | 630.6 | 6.01 / 7.69 / 8.09 | 0.00 | 0.14 |
| contended / 4 / 1 / 1 | sqlite none | 3 | 314.1 | 314.1 | 3.08 / 4.13 / 135.25 | 3.31 | 0.16 |
| contended / 4 / 1 / 1 | sqlite trigger | 3 | 255.2 | 255.2 | 3.98 / 4.89 / 252.85 | 4.97 | 0.22 |
| contended / 4 / 1 / 1 | sqlite explicit | 3 | 285.7 | 285.7 | 3.06 / 4.90 / 157.00 | 4.12 | 0.17 |
| contended / 4 / 1 / 1 | wal none | 3 | 285.8 | 285.8 | 3.14 / 4.15 / 141.67 | 3.48 | 1.60 |
| contended / 4 / 1 / 1 | wal cdc | 3 | 256.0 | 256.0 | 3.99 / 5.98 / 160.90 | 5.12 | 3.08 |
| contended / 4 / 1 / 1 | wal explicit | 3 | 264.9 | 264.9 | 3.97 / 5.09 / 159.04 | 4.71 | 2.30 |
| contended / 4 / 1 / 1 | mvcc none | 3 | 302.2 | 302.2 | 3.03 / 22.99 / 136.00 | 3.95 | 0.13 |
| contended / 4 / 1 / 1 | mvcc cdc | 3 | 26.3 | 26.3 | 94.95 / 104.79 / 148.02 | 7.93 | 0.34 |
| contended / 4 / 1 / 1 | mvcc explicit | 3 | 312.8 | 312.8 | 3.07 / 47.44 / 100.44 | 4.99 | 0.14 |
| replication / 4 / 16 / 1 | sqlite none | 3 | 247.7 | 3963.1 | 4.00 / 4.90 / 165.99 | 4.04 | 0.16 |
| replication / 4 / 16 / 1 | sqlite trigger | 3 | 238.0 | 3808.8 | 4.00 / 5.05 / 172.73 | 4.31 | 0.18 |
| replication / 4 / 16 / 1 | sqlite explicit | 3 | 261.9 | 4191.1 | 3.98 / 4.46 / 208.32 | 3.98 | 0.17 |
| replication / 4 / 16 / 1 | wal none | 3 | 243.8 | 3901.5 | 4.12 / 5.18 / 167.56 | 4.08 | 1.71 |
| replication / 4 / 16 / 1 | wal cdc | 3 | 233.5 | 3736.6 | 4.11 / 41.07 / 249.06 | 6.09 | 4.72 |
| replication / 4 / 16 / 1 | wal explicit | 3 | 234.6 | 3753.8 | 4.06 / 7.98 / 183.04 | 4.69 | 2.38 |
| replication / 4 / 16 / 1 | mvcc none | 3 | 475.1 | 7601.1 | 8.96 / 12.05 / 12.11 | 0.00 | 0.22 |
| replication / 4 / 16 / 1 | mvcc cdc | 3 | 9.6 | 153.2 | 417.03 / 428.71 / 431.06 | 0.49 | 1.12 |
| replication / 4 / 16 / 1 | mvcc explicit | 3 | 482.0 | 7711.5 | 6.94 / 12.03 / 12.14 | 0.00 | 0.22 |
| authority / 1 / 4 / 1 | sqlite none | 3 | 252.0 | 1008.0 | 3.98 / 4.27 / 7.59 | 0.00 | 0.16 |
| authority / 1 / 4 / 1 | sqlite trigger | 3 | 231.3 | 925.2 | 4.00 / 5.08 / 7.82 | 0.00 | 0.27 |
| authority / 1 / 4 / 1 | sqlite explicit | 3 | 230.6 | 922.3 | 4.00 / 6.00 / 8.08 | 0.00 | 0.16 |
| authority / 1 / 4 / 1 | wal none | 3 | 236.7 | 946.7 | 4.00 / 5.97 / 6.06 | 0.00 | 1.52 |
| authority / 1 / 4 / 1 | wal cdc | 3 | 244.0 | 976.0 | 4.00 / 4.62 / 5.49 | 0.00 | 2.10 |
| authority / 1 / 4 / 1 | wal explicit | 3 | 246.8 | 987.3 | 4.00 / 4.12 / 5.09 | 0.00 | 1.73 |
| authority / 1 / 4 / 1 | mvcc none | 3 | 242.7 | 970.7 | 4.00 / 4.09 / 6.50 | 0.00 | 0.14 |
| authority / 1 / 4 / 1 | mvcc cdc | 3 | 12.8 | 51.3 | 75.94 / 88.86 / 92.04 | 0.00 | 0.29 |
| authority / 1 / 4 / 1 | mvcc explicit | 3 | 291.4 | 1165.6 | 3.07 / 4.10 / 4.91 | 0.00 | 0.14 |
| mixed / 4 / 4 / 1 | sqlite none | 3 | 265.4 | 1061.7 | 3.97 / 4.53 / 155.36 | 3.69 | 0.16 |
| mixed / 4 / 4 / 1 | sqlite trigger | 3 | 242.5 | 970.0 | 3.99 / 6.07 / 230.96 | 4.78 | 0.37 |
| mixed / 4 / 4 / 1 | sqlite explicit | 3 | 264.7 | 1058.8 | 3.97 / 4.16 / 161.98 | 4.19 | 0.17 |
| mixed / 4 / 4 / 1 | wal none | 3 | 275.9 | 1103.7 | 3.98 / 4.91 / 208.61 | 4.32 | 1.63 |
| mixed / 4 / 4 / 1 | wal cdc | 3 | 245.1 | 980.4 | 4.01 / 6.10 / 178.05 | 5.17 | 3.33 |
| mixed / 4 / 4 / 1 | wal explicit | 3 | 250.0 | 999.9 | 4.00 / 5.58 / 198.89 | 5.05 | 2.33 |
| mixed / 4 / 4 / 1 | mvcc none | 3 | 569.6 | 2278.6 | 6.05 / 9.05 / 12.20 | 0.00 | 0.15 |
| mixed / 4 / 4 / 1 | mvcc cdc | 3 | 32.6 | 130.4 | 121.97 / 129.79 / 131.86 | 0.36 | 0.41 |
| mixed / 4 / 4 / 1 | mvcc explicit | 3 | 653.3 | 2613.0 | 6.00 / 6.98 / 7.39 | 0.00 | 0.16 |
| catchup / 4 / 4 / 1 | sqlite none | 3 | 257.7 | 1030.6 | 3.98 / 5.02 / 165.34 | 4.03 | 0.16 |
| catchup / 4 / 4 / 1 | sqlite trigger | 3 | 220.2 | 880.8 | 4.00 / 15.97 / 155.56 | 4.33 | 0.37 |
| catchup / 4 / 4 / 1 | sqlite explicit | 3 | 245.2 | 980.7 | 4.00 / 5.19 / 166.47 | 4.17 | 0.17 |
| catchup / 4 / 4 / 1 | wal none | 3 | 251.6 | 1006.4 | 3.99 / 5.04 / 198.85 | 4.43 | 1.63 |
| catchup / 4 / 4 / 1 | wal cdc | 3 | 252.0 | 1008.1 | 3.99 / 5.07 / 167.98 | 5.04 | 3.32 |
| catchup / 4 / 4 / 1 | wal explicit | 3 | 246.5 | 985.9 | 4.00 / 5.19 / 225.97 | 4.69 | 2.34 |
| catchup / 4 / 4 / 1 | mvcc none | 3 | 621.0 | 2484.2 | 6.01 / 8.08 / 9.29 | 0.00 | 0.15 |
| catchup / 4 / 4 / 1 | mvcc cdc | 3 | 30.3 | 121.2 | 130.10 / 146.74 / 152.06 | 0.48 | 0.42 |
| catchup / 4 / 4 / 1 | mvcc explicit | 3 | 639.6 | 2558.2 | 6.06 / 7.13 / 8.41 | 0.00 | 0.16 |
| independent / 4 / 4 / 4 | sqlite none | 3 | 351.0 | 1404.0 | 10.02 / 15.04 / 18.20 | 0.00 | 0.66 |
| independent / 4 / 4 / 4 | sqlite trigger | 3 | 299.5 | 1197.9 | 12.99 / 16.21 / 19.41 | 0.00 | 0.91 |
| independent / 4 / 4 / 4 | sqlite explicit | 3 | 336.6 | 1346.4 | 11.88 / 15.06 / 17.92 | 0.00 | 0.66 |
| independent / 4 / 4 / 4 | wal none | 3 | 337.6 | 1350.3 | 12.00 / 14.92 / 16.02 | 0.00 | 2.73 |
| independent / 4 / 4 / 4 | wal cdc | 3 | 359.6 | 1438.4 | 11.09 / 14.06 / 15.94 | 0.00 | 4.78 |
| independent / 4 / 4 / 4 | wal explicit | 3 | 350.0 | 1399.8 | 11.95 / 14.92 / 15.97 | 0.00 | 3.58 |
| independent / 4 / 4 / 4 | mvcc none | 3 | 330.5 | 1322.2 | 12.01 / 13.31 / 14.20 | 0.00 | 0.50 |
| independent / 4 / 4 / 4 | mvcc cdc | 3 | 33.8 | 135.1 | 119.04 / 127.45 / 131.11 | 0.00 | 0.81 |
| independent / 4 / 4 / 4 | mvcc explicit | 3 | 340.5 | 1362.0 | 11.96 / 13.98 / 14.11 | 0.00 | 0.51 |
| independent / 8 / 4 / 1 | sqlite none | 3 | 300.6 | 1202.5 | 3.89 / 4.08 / 553.18 | 7.47 | 0.16 |
| independent / 8 / 4 / 1 | sqlite trigger | 3 | 193.6 | 774.2 | 11.94 / 81.30 / 416.02 | 13.32 | 0.54 |
| independent / 8 / 4 / 1 | sqlite explicit | 3 | 290.7 | 1162.6 | 3.91 / 4.98 / 596.51 | 8.40 | 0.18 |
| independent / 8 / 4 / 1 | wal none | 3 | 285.2 | 1140.8 | 3.97 / 4.79 / 495.38 | 8.17 | 2.93 |
| independent / 8 / 4 / 1 | wal cdc | 3 | 269.6 | 1078.3 | 3.99 / 28.18 / 508.81 | 9.61 | 6.51 |
| independent / 8 / 4 / 1 | wal explicit | 3 | 277.5 | 1109.9 | 3.98 / 121.63 / 529.70 | 11.81 | 4.48 |
| independent / 8 / 4 / 1 | mvcc none | 3 | 1168.2 | 4673.0 | 6.14 / 8.05 / 9.17 | 0.00 | 0.18 |
| independent / 8 / 4 / 1 | mvcc cdc | 3 | 31.7 | 126.9 | 243.11 / 270.19 / 483.94 | 0.35 | 0.69 |
| independent / 8 / 4 / 1 | mvcc explicit | 3 | 1214.7 | 4858.9 | 6.18 / 7.89 / 8.52 | 0.00 | 0.19 |
| independent / 2 / 16 / 1 | sqlite none | 3 | 273.6 | 4378.0 | 3.96 / 4.27 / 6.01 | 1.30 | 0.16 |
| independent / 2 / 16 / 1 | sqlite trigger | 3 | 245.7 | 3931.0 | 4.00 / 5.11 / 7.78 | 1.46 | 0.55 |
| independent / 2 / 16 / 1 | sqlite explicit | 3 | 265.9 | 4254.4 | 3.97 / 4.18 / 8.00 | 1.27 | 0.16 |
| independent / 2 / 16 / 1 | wal none | 3 | 256.5 | 4103.4 | 4.00 / 5.04 / 6.01 | 1.24 | 1.04 |
| independent / 2 / 16 / 1 | wal cdc | 3 | 235.3 | 3764.9 | 4.17 / 5.20 / 77.40 | 1.80 | 2.36 |
| independent / 2 / 16 / 1 | wal explicit | 3 | 244.6 | 3913.2 | 4.00 / 5.06 / 71.71 | 2.27 | 1.41 |
| independent / 2 / 16 / 1 | mvcc none | 3 | 325.5 | 5207.8 | 6.01 / 6.95 / 7.64 | 0.00 | 0.17 |
| independent / 2 / 16 / 1 | mvcc cdc | 3 | 9.4 | 150.2 | 212.93 / 217.98 / 219.22 | 0.00 | 0.64 |
| independent / 2 / 16 / 1 | mvcc explicit | 3 | 325.6 | 5208.8 | 6.01 / 7.00 / 7.06 | 0.00 | 0.17 |
| independent / 4 / 1 / 1 | mvcc cdc | 3 | 77.5 | 77.5 | 49.05 / 61.00 / 64.02 | 0.33 | 0.24 |

## Failures

- mvcc-cdc-independent-w4-b1-p1-r0: 1; Error: Locking error: Failed locking file '/Users/david-bieregger/RustroverProjects/rocky-db/benchmarks/engine-comparison/results/databases/mvcc-cdc-independent-w4-b1-p1-r0/part0.d
- lag-consumer-wal-cdc-r0: 1; Error: database is locked
        0.15 real         0.02 user         0.01 sys
            18939904  maximum resident set size
                   0  average shared memory size
    
