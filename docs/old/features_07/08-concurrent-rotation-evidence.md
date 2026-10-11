# Phase 8: concurrent rotation evidence

The implementation is described by
[decision 0050](../decisions/0050-concurrent-key-epochs.md). Tests use the existing
[scripts/test-tiers](../../scripts/test-tiers) classification and budgets. No
exception or enlarged budget was added. The existing concurrent catch-up writer
fixture now uses 192 operations (six receive pages), retains cross-log pending
dependencies and concurrent SQL commits, and uses the normal 2-second budget;
its previous 6-second exception was removed. The 10,000-operation soak remains
separate. The existing SQLite-header file probe creates and reads a real file;
it is classified as integration rather than a pure unit test.

## Completion coverage

| Gate | Evidence |
| --- | --- |
| Identity order in both arrival orders, up to four siblings | `rocky-core`'s `up_to_four_siblings_choose_the_lowest_hash_in_every_arrival_order` enumerates every permutation for 16 sets of one to four siblings, against the lowest-hash reference |
| Acceptance 3 for a collection | SDK `concurrent_collection_rotations_open_both_rows_and_an_offline_writer_renews_once`: two issuers create distinct generation-2 keys, open both histories, converge at generation 3, then remain there for ten sync rounds |
| Acceptance 3 for three group tables | SDK `concurrent_group_creation_opens_every_table_and_converges_after_one_sync_each`: independent first-generation secrets, one sync each, both rows in each table, equal identities and winners, subsequent writes from both issuers |
| Concurrent revocations of actual readers and the exposure window | SDK hub `two_revoked_list_readers_open_the_opposite_sibling_but_not_the_superseding_rows` enrolls Carol and Dave, revokes them independently, and lists Carol’s rows `b, before` and Dave’s `a, before`; neither receives generation 3 or opens `after`. SDK `concurrent_reader_revocations_supersede_both_exposures_and_equal_statements_stop_rotating` also uses two issuer vaults, two recorded readers of list L and the real sealed epoch exchange. Secure `concurrent_revocations_list_the_exposure_window_and_close_it_at_the_superseding_writer` verifies the actual ciphertext each reader's held secrets open: Carol rows 1 and 3, Dave rows 1 and 2, neither rows 4 and 5 under generation 3 |
| Non-reader revocation leaves epochs unchanged | The SDK test above records a custody device, merges its revocation at both issuers and verifies neither rotates the collection or group; phase 7's registry regression tests also run |
| Same statements, simultaneous superseding epochs terminate | Both issuer vaults independently create generation 3 with the merged statement IDs; ten rounds of authenticated, sealed deliveries retain the same winner and refuse to start another rotation |
| Offline writer renews once | The collection hub test disconnects the writer's endpoint before rotations, reopens it after convergence, renews once and writes with generation 3 |
| Group rotation recovery | The `reopen_completes_each_revocation_{secret,table,commit}_boundary` tests sweep the saved intent, eight secret-store boundaries, each of six member-table installations in two three-table partitions, signature/renewal/tombstone/commit and intent cleanup. Two real reopens retain the statement, secrets and table identities |
| Received sibling recovery | `received_sibling_resumes_after_each_table_installation` stops at each of three tables and uses the real database open; every table holds both siblings and the same winner |
| Retention and checkpoints | Secure `siblings_open_both_histories_converge_writers_and_retire_after_pruning` checks pending receipt without the sibling, both histories, winner writes and checkpoint offers, refusal to retire retained history, then pruning and successful retirement |
| Capacity | Secure `the_257th_sibling_is_refused_without_changing_the_winner` installs 256 siblings and verifies the next installation's capacity error and unchanged identities. The SDK group capacity test also counts siblings |
| Current formats only | Frame, bundle payload, checkpoint, file, identity and group-envelope refusal tests reject other versions; group tests authenticate the hash and statement IDs |
| Packaged consumer types | Kotlin `KeyEpochTest`, `RoomScalarsTest.groupPartitionSharesOneSecretForEveryTable`, and the shopping example's Room/SDK tests exercise complete identities |

The pure identity and SDK group-bound tests are unit tests. SQLite, encryption,
delivery, hub and recovery tests are integration tests under the existing
package filters. The previous complete file-provider rotation sweep and the
partitioned-issuer revocation scenarios retain their e2e tier.

## Verification

Verified on 2026-10-10 in `p08-concurrent-rotation`:

- `./scripts/lint`: exit 0.
- `NEXTEST_TEST_THREADS=4 ./scripts/check`: exit 0; formatting and Clippy passed,
  including the SDK without default features. Unit: 133 passed, 207.5 ms suite
  wall time against 10 s. Integration: 733 passed, 141.55 s against 300 s.
- `./sdks/native/check`: exit 0; 2 tests passed, bindgen Clippy and the build
  without default features passed.
- Targeted SDK end-to-end regressions: 4 passed in 8.206 s (partitioned issuers
  in both sync orders, interrupted file-provider group rotation, independent
  issuer lists). The last scenario also passed its integration budget above.
- Kotlin `rocky-core`: 5 tests passed; `schema-qualification`: 5 passed.
- `./examples/shoppinglist-app/check`: the schema preflight, 28 shared tests
  and desktop build passed.

Representative Phase 8 measurements, under the runner's existing isolated
rerun/load-scaling policy (including its retained passing measurements):

| Test | Time | Budget |
| --- | --- | --- |
| Identity permutations | 0.1 ms | 10 ms |
| SDK group epoch capacity | 1.7 ms isolated | 10 ms |
| Collection hub convergence and offline renewal | 1.06 s isolated | 2 s |
| Three-table group hub convergence | 678 ms isolated | 2 s |
| Two enrolled revoked readers on the hub | 1.09 s isolated | 2 s |
| Simultaneous superseding epochs and ten sealed exchanges | 163 ms | 2 s |
| Secure exposure-window ciphertext proof | 181 ms | 2 s |
| Group rotation recovery sweep | 1.19 s isolated | 2 s |
| Received-sibling recovery | 200 ms isolated | 2 s |
| Sibling retirement/checkpoint proof | 70 ms isolated | 2 s |
| Secure 257th sibling refusal | 51 ms | 2 s |

The optimized concurrent catch-up writer test passed in 1.925 s with its former
exception removed. Existing Phase 3 exceptions for other baseline tests remain;
none was added for Phase 8.

The hub tests use memory transports and an in-memory secret provider; rotation
and received-sibling recovery use real SQLite files and real reopen. The shopping
check compiles the desktop app and tests Room on native Rocky. This is not a
physical-device deployment, live-network mesh qualification, complete e2e/soak
suite or cryptographic audit.
