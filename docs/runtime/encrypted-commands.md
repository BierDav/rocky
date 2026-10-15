# Encrypted coordinated writes

Encrypted coordinated writes are ordinary signed, encrypted row bundles.
Trusted nodes store each bundle before acknowledging it; the metadata-only
coordinator orders entries and decides commitment. Row grants and reader keys
continue to govern access. Being trusted by the coordinator grants no row access.
There are no encrypted command envelopes, preparations or result forwarding.

Use [coordinated writes](coordinated-writes.md) for the supported contract,
APIs, failure handling and limits. [Phase 11 evidence](../features_09/11-removing-strong-commands-evidence.md)
tracks the remaining consumer conversions and acceptance gates.
