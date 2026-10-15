# Rotating author pseudonyms

Status: obsolete; [phase 13](../features_09/13-identities-and-domain-credentials.md)
removes pseudonyms.

## Background

A table that is referenced from another scope
exposes an existence log to every device of the domain: one record per row
created or deleted, without column values. Each record and each row `id` names
its author. To keep that from naming a device, the author is a pseudonym: a
signing key and an author number that the issuer assigns per device and table.

## Problem

A pseudonym that never changes can be linked to a person once and then
followed indefinitely:

- A colleague who knows that Anna created one customer reads her author number
  from that row's `id`, and with it all her activity in that table.
- In a small team, timing alone can identify a person.

## Idea

A device replaces its pseudonym regularly, so that a link only holds for one
period.

- The issuer assigns a fresh signing key and author number, for example with
  each key epoch or at each credential renewal.
- Rows created earlier keep their `id`. Only new rows carry the new author
  number.
- Only the issuer can connect the pseudonyms of one device, so accountability
  is kept.

## Cost

- Every pseudonym leaves one permanent number per partition it wrote to: the
  highest sequence receivers have seen from it. More rotation means more of
  these.
- Author numbers are consumed and never reused. The bit split of the `id`
  bounds how often a domain can rotate.
- A device needs to reach an issuer to rotate. Issuing several pseudonyms in
  advance would let an offline device rotate by itself.

## Open

- The period: per key epoch, per renewal, or per time span.
- Whether an old pseudonym is closed with a signed statement, so that a stolen
  old key cannot create rows.
- Whether rotation should also be per partition. That would hide that the same
  device writes to two partitions, but the issuer cannot enumerate partitions
  behind a selector such as "any warehouse".
- This is pseudonymisation, not anonymity. Whether it satisfies a works council
  or data-protection review is a legal question outside this design.
