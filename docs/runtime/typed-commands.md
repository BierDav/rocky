# Coordinated application decisions

Application decisions use SQL on ordinary rows inside a coordinated write.
There are no command descriptors, handler generations, invocations or tickets.
Applications own validation and business transitions. The runtime stages and
durably stores the bundle, then applies and publishes the coordinator's committed
history. Competing writes retry after catching up to the winner.

Use [coordinated writes](coordinated-writes.md) for the supported contract,
APIs, failure handling and limits. [Phase 11 evidence](../features_09/11-removing-strong-commands-evidence.md)
tracks the remaining consumer conversions and acceptance gates.
