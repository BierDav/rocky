# Rotating author pseudonyms

Status: obsolete. [Phase 13](../features_09/13-identities-and-domain-credentials.md)
removed pseudonyms
([decision 0075](../decisions/0075-generations-as-sealed-forks.md)); there is
nothing left to rotate.

## What replaced them

Permissions belong to [identities](../runtime/credentials.md#identities).
Each node signs every frame, existence record and checkpoint with its own
node key, under a certificate its identity signed, and creates rows under
one author number from its identity's range
([node authors](../runtime/credential-primitives.md#principal-ranges-and-node-authors)).
A node needs no issuer to write any table it is granted, and nodes of one
identity never collide.

## Consequence

Frames and [existence logs](../runtime/existence-logs.md) name their node to
every device of the domain that can see them, so a reader can link a node's
activity across tables and partitions. Rocky does not hide it within a
domain. An application whose data must not be linked keeps it in a separate
domain.

## Rejected

Rotating per-table keys and numbers would need an issuer per node, table and
period, which defeats self-enrollment and adoption without an issuer, and
would only pseudonymise, not anonymise. A cloned node key stays a
[deferred optimization](../optimizations_on_demand/cloned-node-detection.md).
