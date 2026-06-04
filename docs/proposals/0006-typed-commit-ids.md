# Proposal: typed commit ids internally, `RevisionSummary` for display only

## Motivation

The unit of currency between modules is `RevisionSummary` (`src/model.rs:3`):

```rust
pub struct RevisionSummary {
    pub expression: String,        // a revset string, e.g. "@", "origin/main"
    pub resolved: Option<String>,  // a commit id hex, sometimes
    pub exact: bool,
}
```

It is passed into `merge_revisions`, `is_ancestor`, `read_entries_at_rev`,
`checkout_revision`, `complete_sync`, and more. It is doing two jobs at once: it
is both "the revset the user typed" and "the commit we resolved it to."

## Problem statement

Because `resolved` is optional, identity comparison is ambiguous:

```rust
pub fn same_revision(&self, other: &Self) -> bool {
    match (&self.resolved, &other.resolved) {
        (Some(left), Some(right)) => left == right,
        _ => self.expression == other.expression,   // string compare fallback
    }
}
```

When either side is unresolved, this compares raw revset strings. Two
`RevisionSummary`s both carrying `expression = "@"` are treated as the same
revision even if they were produced at different points and `@` moved between
them. `sync` and `merge_revisions` lean on `same_revision`
(`src/jj.rs:34,364,479`) to decide whether to skip a checkout or reuse a side of
the merge, so a wrong equality answer here silently changes behavior.

Every `JjClient` method that accepts a `RevisionSummary` also has to re-resolve
it through `resolve_summary_to_commit` (`src/jj.rs:589`), which branches on
whether `resolved` is `Some`. That is repeated work and repeated branching for a
value that, once resolved, should never need re-resolving.

## Proposal

Separate the resolved identity from the display string.

Use jj-lib's `CommitId` as the internal handle for "a specific commit," and keep
`RevisionSummary` purely for human-facing output.

```rust
// internal handle, always points at exactly one commit
#[derive(Clone, PartialEq, Eq)]
pub struct Revision {
    id: CommitId,
    label: String, // the revset/bookmark it came from, for messages only
}

impl Revision {
    pub fn id(&self) -> &CommitId { &self.id }
    pub fn same(&self, other: &Revision) -> bool { self.id == other.id }
}
```

`resolve_rev`, `root_revision`, `current_revision`, and `bookmark_summary`
return `Revision`. All the methods that today take `&RevisionSummary` and
re-resolve it instead take `&Revision` and use `id()` directly — deleting the
`resolved.is_some()` branch in `resolve_summary_to_commit`.

`RevisionSummary` is reduced to a display DTO produced from a `Revision` only at
the status-printing boundary (`src/status.rs:146`), carrying the short id and the
expression. It loses `same_revision` entirely; equality is `CommitId` equality
on `Revision`.

### Why not keep one type

The optional `resolved` is the whole problem: it makes "are these the same
commit?" answerable only sometimes, and answerable *wrongly* the rest of the
time via string fallback. Splitting the type makes the resolved case
non-optional and pushes the unresolved case (a raw user revset) to exactly one
place: the resolution call that turns it into a `Revision`.

### Sequencing with other proposals

This pairs naturally with 0001 (single session): once a session holds the loaded
repo, a `Revision` is just a `CommitId` plus a label, and resolution happens once
per command. Land 0001 first, then this, so the `&self`/re-load churn is already
gone when ids become typed.

## Non-goals

- Not introducing a newtype over `CommitId`; reuse jj-lib's type directly.
- Not changing user-facing revset syntax or status output format.
- Not removing the `exact` field here — proposal 0003 already deletes it.

## Verification

- `same_revision`'s string-fallback branch no longer exists; grep confirms no
  expression-string equality remains in identity decisions.
- `merge_revisions` ancestor short-circuits and the `checkout_revision` skip in
  `sync::run` (`src/sync.rs:34`) are driven by `CommitId` equality.
- All `tests/dotmerge_cli.rs` cases pass, including
  `sync_reuses_import_without_merge_when_target_is_ancestor` and
  `sync_reuses_target_without_preserving_disposable_empty_at`, which depend on
  correct "same revision" decisions.

## Success criteria

- Internal APIs pass `Revision` (resolved) rather than a maybe-resolved summary.
- Revision identity is `CommitId` equality, never a revset-string compare.
- `RevisionSummary` exists only at the display boundary.
