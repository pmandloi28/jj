# Fileset-Based Sparse Checkouts (`jj sparse`)

Author: [Priyanka Mandloi](mailto:mandloip@google.com)

## Summary

This document proposes transitioning `jj sparse` checkouts and working-copy
pattern tracking from simple directory prefix matching (`Vec<RepoPathBuf>`) to
`jj`'s central `FilesetExpression` DSL engine. This unifies sparse patterns
with `jj`'s standard pattern query system, resolves command paths relative to
the current working directory (`CWD`), and establishes a clean, canonical DSL
string format for storing sparse configurations.

## State of the Feature as of 0.25.0

Previously, `jj` tracked sparse checkouts as an unordered list of directory
prefixes relative to the repository root. This model had several UX and
technical limitations:

1. **Root-Relative Constraints:** All CLI paths passed to `jj sparse set --add`
   or `--remove` had to be manually resolved or typed relative to the
   repository root. Specifying a relative directory like `../sibling` from
   inside a subdirectory resulted in an error or checked out the wrong folder.
2. **No Globbing or Exclusions:** Users could only select directory branches.
   It was impossible to target specific file extensions (e.g. only `.rs`
   files), exclude specific subfolders within an included folder, or use
   pattern operators like union (`|`) and difference (`~`).

## Goals and non-goals

### Goals
* Use `jj`'s standard fileset DSL (`FilesetExpression`) to
  define and evaluate sparse checkouts.
* Support directory-relative path parsing across all `jj
  sparse` subcommands, so patterns like `./file` or `../dir` resolve relative
  to the user's terminal location.

### Non-goals
* Renaming repository paths to different locations in the
  working folder (e.g. mapping `repo/foo` to `wc/bar`) is out of scope and
  belongs to the broader [Sparse Patterns v2 Redesign](sparse-v2.md).
* Normalizing complex logical expressions
  (e.g., simplifying `(A & B) | (A & C)` to `A & (B | C)`) is not required for
  correctness and is deferred.

## Detailed Design

### 1. Storage & Schema Migration

The workspace's `SparsePatterns` metadata is updated to store fileset
expressions inside a new `fileset_expression` field. The legacy `prefixes` list
is preserved to maintain compatibility:

```protobuf
message SparsePatterns {
  repeated string prefixes = 1;      // Preserved for backward-compatibility
  string fileset_expression = 2;     // e.g. "(src/ & ~src/temp/) | glob:*.rs"
}
```

When loading older repositories:
* If the `fileset_expression` string is empty, `jj` reads the legacy directory
  prefixes, converts each prefix into a root-relative directory match (e.g.
  `root:"dir"`), and unions them.
* A legacy prefix list containing only `""` (matching all files) is cleanly
  upgraded to `all()`.

#### Rationale: Storing AST as DSL String vs. Structured Protobuf Nodes

Instead of defining a deeply nested, recursive Protobuf schema that mirrors the `FilesetExpression` AST structure (e.g., recursive `oneof` messages representing unions, intersections, and glob models), we chose to serialize the AST into a single canonical DSL string. 

This approach offers several key advantages:

1. Storing a string keeps the `.proto` schema simple.
2. To support `jj sparse edit`, we must format the AST into a text string for the editor, and parse the user's text edits back into an AST. Storing the AST as a string on disk allows us to reuse these exact same serialization (`to_string()`) and parser (`fileset::parse()`) code paths, ensuring 100% format consistency and eliminating duplicate conversion logic.

### 2. DSL Syntax & Serialization

To save the sparse state to disk and display it to the user in a consistent
way, the `FilesetExpression` AST is serialized back to a canonical DSL format
using root-relative path markers:

* Exact file matches are formatted as `root:"path/to/file.rs"`
* Directory matches are formatted as `root:"path/to/dir"`
* Glob patterns are formatted as `root-glob:"src/**/*.rs"` or
  `root-prefix-glob:"cli/src/commands/*.rs"`

#### Precedence-Aware Formatting
To prevent ambiguous syntax when mixing operators, parentheses are automatically
omitted or emitted based on standard operator binding rules (where `Difference`
`~` has higher precedence than `Intersection` `&`, which has higher precedence
than `UnionAll` `|`).

For example:
* The AST representing `"A or B, but not C"` is serialized cleanly as
  `root:"A" | root:"B" ~ root:"C"`.
* The AST representing `"A, and also B but not C"` is serialized as
  `root:"A" | (root:"B" ~ root:"C")` to preserve evaluation boundaries.

### 3. User Flows & Command Behaviors

Every `jj sparse` subcommand is updated to parse patterns relative to the
user's active shell directory.

#### Modifying Patterns via `jj sparse set`
Users can pass direct fileset expressions as positional arguments:
```console
$ jj sparse set 'src/**/*.rs | README.md'
```

When using `--add <FILESETS>` and `--remove <FILESETS>` flags, the paths are
resolved relative to the terminal directory, and combined with the existing
sparse rules using union (`|`) and difference (`~`) operators:
```console
$ cd cli/src/
$ jj sparse set --add ../tests --remove ./commands/sparse
# Resolves to: (current_patterns | root:"cli/tests") ~ root:"cli/src/commands/sparse"
```

If the resulting expression simplifies to `none()` or `all()`, the command
optimizes the state before writing changes to disk or materializing files in
the working directory.

#### Editing Patterns via `jj sparse edit`
`jj sparse edit` opens the user's `$EDITOR` populated with the current active
fileset expression.

If the user introduces a syntax error, the command displays visual caret
diagnostics pointing to the exact character error and aborts immediately. This
maintains the standard "fail-fast" behavior of other interactive commands,
preventing the working copy from becoming locked or corrupted.

#### Listing Active Patterns
`jj sparse list` (and its new alias `jj sparse show`) displays the current
fileset expression formatted in canonical DSL syntax:
```console
$ jj sparse list
root:"cli" | root:"docs" ~ root:"cli/tests"
```

### 4. Working Copy Materialization

During checkouts, updates, or merges, the working copy uses the active
`FilesetExpression` to construct an optimized path matcher. This matcher
filters the tree difference stream, ensuring only matching files are added,
modified, or removed on disk.

## Alternatives considered

### 1. Tracking Rules via explicit Include/Exclude Lists
We considered changing the internal storage format to an ordered list of rule
structs, e.g., `Vec<SparseRule { path: RepoPathBuf, include: bool }>`, which
would serialize as a list of lines prefixed with `include:` or `exclude:`.
* **Why it fell short:** This creates a secondary pattern-matching language
  specific to sparse checkouts. It duplicates path resolution and glob
  evaluation logic, and fails to leverage `jj`'s existing fileset parser and
  query engine.

## Related Work

* **[Sparse Patterns v2 Redesign](sparse-v2.md):** The original sparse-v2
  proposal outlines moving sparse patterns into the operation store (`OpStore`)
  via a `WorkingCopyPatternsId`. Transitioning to `FilesetExpression` serves as
  the exact concrete query language and evaluation engine required by the v2
  design.

## Future Possibilities

### 1. Two-Set Normalization (`(pos) ~ (neg)`)
If a user repeatedly appends additions and exclusions, the internal AST can
accumulate deep nested difference chains (e.g. `(((A | B) ~ C) | D) ~ E`).

To keep the expression clean, we can introduce a normalization layer that
automatically flattens any fileset expression into a simplified "two-set"
representation: `(positives) ~ (negatives)`.

When a user runs `--add` or `--remove`, the changes are merged directly into
either the positive or negative branch:
* Adding a pattern appends it to the positive branch and removes it from the
  negative branch.
* Removing a pattern appends it to the negative branch.

This keeps the serialized patterns compact and easy to read.
