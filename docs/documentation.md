# Documentation conventions

## Location and audience

Prefer `docs/` for explanations, design rationale, workflows and extended examples.
Keep the root README as an entry point and `AGENTS.md` as actionable instructions.
Benchmark-specific reproduction and evidence stay beside the benchmark.

Write for one reader task at a time. State whether a document describes proposed,
implemented or historical behavior. Link to existing explanations instead of
copying them into multiple files.

## Length and splitting

Around 200 lines, consider splitting the document. A reader often needs only one
part of a large guide. This is a review signal, not a hard CI failure.

Split by independently useful topic or task: setup, recovery, mutation contracts
or observation. Retain a short entry page with links when useful. Each resulting
document must explain enough context to be read on its own.

Do not split mechanically at a line number, create unnecessary tiny files or
compress prose just to satisfy a count. Check links after moving sections.
Versioned proposals and historical evidence may remain longer when preserving
their context matters; reconsider their structure before substantial expansion.

## Code comments

Keep comments short. Explain an invariant, a surprising constraint or why an
ordering matters. Do not narrate obvious statements or put essays in source files.

Concise API documentation and small examples can stay with the API. Extended
tutorials and rationale belong in `docs/`, linked from source when helpful.

## Navigation and maintenance

Maintain responsibility-level guides, not descriptions of every file. Use Cargo
metadata and scoped source searches for exact paths and dependency information.

When changing a contract, update its owning guide. When moving documents, repair
incoming and outgoing relative links. Add new pages to the nearest relevant index
only when they provide a useful entry point.

Record substantial architectural decisions in [decision records](decisions/README.md).
Routine fixes and refactors do not need a new decision record.
