# Ordinary tables for coordinated decisions

Tables written by coordinated decisions are ordinary mergeable tables. SQL,
drivers and Room read their committed row projection. There is no
command-controlled or confirmed-read-only table class. Definitions naming either
removed class are refused. Applications enforce their coordination discipline;
Rocky does not prevent a trusted application from making an ordinary SQL write.

Use [coordinated writes](coordinated-writes.md) for the supported contract,
APIs, failure handling and limits. [Phase 11 evidence](../features_09/11-removing-strong-commands-evidence.md)
tracks the remaining consumer conversions and acceptance gates.
