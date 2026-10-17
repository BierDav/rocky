# Schema changes

A schema change is a [seal](tips-and-bookmarks.md): the tip after it has
another schema. [Decision 0075](../decisions/0075-generations-as-sealed-forks.md)
records the model; [following schema changes](adopting-generations.md)
covers members, [removal](removal.md) removed and re-added names. Schemas
have no chain of transitions.

## Choose the change

- Local version, SQL spelling, local tables and indexes do not change the
  replicated definition. A local plan keeps the manifest and row identities.
- Adding a mergeable table or a nullable or defaulted field is additive.
  Names are the identities of tables and fields
  ([decision 0067](../decisions/0067-names-are-identities.md)).
- Requiring a field of an existing table, newly added or once nullable, is
  additive too: the field needs a default, and rows that lack it or hold
  NULL read the default ([required fields](#required-fields)).
- Removing mergeable tables or fields needs
  [application confirmation](#confirmed-removals); renaming is removal and
  addition. Key or type changes, reader-scope remapping and partition column
  or group changes are refused.

`rocky_core::classify` rejects unsupported changes before anything is
signed. The schema commitment of a tip is the hash of its
`ApplicationManifest`, which includes each table's `granted_like`.

## Publishing

An application follows a bookmark it names, by convention `main`; Rocky
creates none. The owner's application calls `create("main")` and
`follow("main")` once after creating the domain, and a domain that migrates
signs its genesis line's minimum once (`genesis_minimum`). Enrollment
carries the inviter's graph, its followed bookmark and that tip's schema
record, so a joiner follows the same bookmark.

`Database::open` compares the declared schema D (version Vd) with the
followed tip's installed manifest M (generation Vi):

| Condition | `application` | Opening does |
| --- | --- | --- |
| D at Vi hashes to M | `current` | A local rebuild if needed |
| Vd > Vi | `ahead` | An issuer, owner or trusted identity publishes; a member waits (`waiting_for_issuer`) |
| Otherwise, the same version included | `behind` | Opens the installed schema; never publishes |
| Following no bookmark | `ahead`, `reason: follow_required` | Never changes schema |

Publishing happens before the network starts, when the followed bookmark
points to an open tip:

- **Additive or removal, no migration above Vi:** a `continue` seal naming
  the new schema and a forward bookmark move. A removal is
  [confirmed](#confirmed-removals) first.
- **With migrations:** the issuer needs [full access](#migrations); it
  closes the tip, opens a migration successor on the close and moves the
  bookmark ([migrations](#migrations)).

A coordinated `continue` is the source log's terminal entry, as for any
seal. Issuers that publish one tip apart resolve by authority, then digest;
the other tip stays a branch, listed as `main/1`, and its writes return as
[held changes](held-changes.md) on the device that made them.

## The switch

`EncryptedReplica::switch_schema` installs the followed tip's authenticated
schema. An additive change applies in place: `ALTER TABLE … ADD COLUMN` and
`CREATE TABLE` in the follow transaction, with no backfill, no change to
stored rows and no observer event; sessions and cursors stay valid. A
change to a constrained table (unique policy, foreign key) or one whose path
filters stored rows recreates the tables through a projection plan, which
fills shadow tables 256 rows per transaction and switches in one verified
transaction. Removals and migrations keep "tables rebuilt" notifications and
stale sessions. The plan's final transaction moves the workspace to the tip;
reopening resumes it.

## Confirmed removals

An issuer opening a schema that removes tables or fields receives
`RolloutConfirmationRequired` before settings, secrets or the replica
change, and before its network starts. Its `report` lists removed tables
and `table.field` names with the live stored row count of each table on
this copy, including rows suppressed by constraints and every held
partition. The application shows the report and calls
`Database::open_confirmed(open, digest)`; Kotlin passes
`confirmation = report.digest`. The digest binds the domain, installed
manifest, complete target schema and counts; a mismatch returns the current
report with `mismatch = true` and changes nothing. Pure additions need no
confirmation. Members never confirm: following a removal another device
published needs none.

## Tables granted like another

The SDK schema's `granted_like` maps a new table to an existing one;
Kotlin declares `grantedLike = "items"` on the table. Wherever the SDK turns
a profile into grants or asks whether it reads a table, a whole-row grant
naming `items` also names `item_categories` with the same rights, and
partition selectors on `items` select its partitions. A follower receives
the new grants and keys with its next renewal, which it asks an issuer for
once per such schema. The entry is part of the manifest and is declared
with the table: changing it for an existing table is refused.

## Required fields

A field rows of the installed schema lack, or hold NULL in, may become
required; it declares a default. Every device reads the default wherever a
row lacks the field or holds NULL, and the switch rewrites stored NULLs the
same way. A device whose application predates the field keeps syncing; its
new rows read the default. The schema's `migrations` compute its values
once ([migrations](#migrations)).

## Migrations

[Decision 0071](../decisions/0071-staged-migration-publication.md) records
the staged publication; phase 17 runs it on the migration tip:

1. The issuer may migrate only if it holds the key of, and reads and writes,
   every partition the domain knows; otherwise status reports
   `full_access_required`.
2. Outside any transaction it signs the `close` seal of the source tip, the
   opening of the migration tip (its schema, the line's signed minimum) and
   the bookmark move.
3. One stage transaction records them as private statements, freezes the
   tables migration steps name as `parent.<table>`, and switches the
   workspace to the new tip. Writes on the source tip past the close's
   frontier become held changes there.
4. At open, before the network starts, it runs every migration above the
   installed version in one SQL transaction of any size on the new tip. A
   step is SQL, or `{"insert": table, "select": sql}`, which inserts a row
   per selected row with an `id` Rocky issues; `parent.<table>` reads the
   frozen source state read-only, and migration SQL cannot call
   `rocky_next_id`. The commit records the publication
   (`rocky.sdk.migration.v3`, the target tip) and its bundles.
5. The commit journals an exact descriptor and disk-backed inclusion proofs.
   The opening identity endorses that intent outside the transaction. An
   uncoordinated stage releases after endorsement. A coordinated one
   stays private until a trusted node that follows the source tip ends its
   log with exactly the close (`rocky.sdk.staged-close.v1`) and enough
   trusted nodes acknowledge it; the issuer verifies that finality and
   releases the statements in one transaction.

Stored frame reconstruction may reuse one successfully parsed descriptor per
replica, bound to the complete freshly selected bytes, with a 32 KiB retained
raw-byte cap. SQL, proofs and current authority remain fresh; this is only
canonical syntax reuse. See the [bounded experiment](../features_09/17-migration-descriptor-parsing-evidence.md)
for its memory and qualification limits.

A crash before the migration commits runs it again; after it, nothing runs
again. A migration that fails while it runs withdraws its private stage
atomically and switches back to the source tip; nothing of it left the
device, and a later application stages its own. An uncertain coordinated
submission keeps the exact stage and retries; it is never withdrawn.

The bundles leave by ordinary sync until peers hold them. Their exact initial
frames carry issuer-endorsed migration membership proofs and use a separate
durable history account; ordinary edits retain the 64 MiB hard limit. A generic
publication receives no exemption. An unavailable external signer leaves the
committed candidate private; reopening signs the same intent without rerunning
SQL. Keyless custody verifies membership without decrypting or reporting applied
rows. See [the acquisition and accounting contract](../decisions/0071-staged-migration-publication.md). Status `migration`
reports `{"state": "publishing", "bundles": n, "published": k}`. A migration
admits no late frames: writes on the source tip past the close return as
[held changes](held-changes.md) with their original source images, and older
applications re-author them as their developers
choose. Issuers that migrate apart race: one tip wins and the other's
migration is never merged, so rows an `insert` step adds appear once.

## Evidence

- SDK `tests/generations.rs`: following a published schema while behind;
  late writes across a `continue`; an old application across a migration
  re-authoring its held write; required and nullable fields; a crash on
  either side of the switch; fetching a required schema from a member; two
  issuers publishing apart (`main/1`); a dev branch whose schema a device on
  `main` never fetches; an offline migration published on reconnect.
- SDK `tests/migrations.rs`: a migration refused before signing, one that
  fails and withdraws, a committed one never repeated, issuers migrating
  apart without duplicated rows, and the 100,000-row gate (e2e).
- SDK `tests/coordinated.rs`
  `an_offline_coordinated_migration_is_released_by_the_exact_close_s_finality`.
- `rocky-secure` `tests::generations::switches` and `tests::tip_schemas`
  (in-place additive switch: rows, observers, cursors, sessions).

## Interrupted schema switches

A durable switch intent records its source, target and bookmark before the
projection plan finishes. Reopen with the source application's schema while
that intent is staged. Opening with the target schema is refused before writes;
the identical switch request resumes, while a different intent must first be
withdrawn.

While a switch is staged the source stays installed, also after a failed plan
in the same process, and the intent is the only authority: admission never
follows the bookmark normally. If the bookmark still continues from the staged
target along the winning seal chain, the intent stays; resuming finishes at the
target and advances to the bookmark's tip in the same transaction, or leaves a
pending switch when that tip has another schema. If the bookmark moved
sideways or back, was deleted, or the target no longer resolves (for example
revoked), the switch is withdrawn in one transaction: the intent, plan and
shadow tables go, the domain's rows are rebuilt from their unstripped bases
and history under the source schema, and the bookmark is followed where it now
resolves. Seal anchors of removed tables are cleared only when a switch
finishes. Never-offered writes the staging held and voided stay held: their
void records are signed facts. This device's own uncommitted publication is
withdrawn only through `withdraw_staged_schema`, together with its intent. An
SDK that crashed between the replica commit and its settings file adopts the
target schema when the committed tip is the pending one or continues from it
with the same schema.

Open and full audit still replay the source's signed history. They then apply
the staging row transform derived from the stored intent, graph and target
schema, including removed tables, and compare the transformed rows and digest
with stored state. Audit and staging share that transform. A tampered row is
still refused; finalization waits until the switch finishes.
