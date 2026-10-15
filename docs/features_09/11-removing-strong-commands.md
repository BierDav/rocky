# 11. Removing strong commands

Status: in progress; acceptance not met. See [evidence](11-removing-strong-commands-evidence.md).
Owner: `rocky-core` definitions and classes; `rocky-secure` command modules;
`rocky-coordinator` command operations; `rocky-sdk`; `sdks/kotlin`; the
shopping and market examples. Depends on [phase 9](09-coordinated-writes.md).
Follow the [execution contract](README.md#agent-execution-contract). Shares decision record 0073.

## Inputs and objective

Read [typed commands](../runtime/typed-commands.md),
[encrypted commands](../runtime/encrypted-commands.md) and
[confirmed tables](../runtime/confirmed-tables.md).

[Coordinated writes](09-coordinated-writes.md) cover what strong commands
did, with ordinary rows. After this phase strong commands and everything that
exists only for them are gone, so schemas no longer carry command versions
that must stay byte for byte forever.

## Settled

- **Removed.** `TableClass::CommandControlled`, `CommandSet`,
  `classify_commands` and command descriptors in manifests; typed and
  encrypted commands, preparations, results, receipt evidence and result
  forwarding; the coordinator's `Acquire`, `Takeover`, `Head`, `Confirm` and
  `Lookup` with their receipt retention; Kotlin `CommandSet`,
  `TableClass.CommandControlled` and `ConfirmedRowsTest`.
- **Shopping approvals** become request rows and a coordinated decision; the
  first committed decision stands.
- **Market restock** becomes a coordinated write.
- **Guides.** The coordinated-writes guide replaces typed commands,
  encrypted commands and confirmed tables; strong history and publication,
  the module map and the command entries of
  [optimizations on demand](../optimizations_on_demand/README.md) follow.

## Ordered slices

1. Port the shopping approvals and the market example to coordinated writes.
2. Remove the command code, classes and coordinator operations with their
   tests.
3. Guides and the module map.

## Completion gate

- No crate, SDK or example references commands or command-controlled tables.
- Two trusted devices deciding the same approval concurrently end with one
  decision on every device.
