# Fileset-Based Sparse Checkouts (`jj sparse`)

Author: [Priyanka Mandloi](mailto:mandloip@google.com)

## Summary

This document proposes making sparse patterns a `FilesetExpression` - the
same expression type used everywhere else in jj - instead of a bare list of
prefixes. A sparse "pattern" becomes any fileset expression, so a working
copy can be defined declaratively as, for example, "everything under `src`",
"everything except `build`", or "every file matching `**/*.md` unioned with
`docs`". 

## State of the Feature as of 0.25.0

Sparse patterns today are a flat and unordered list of directory prefixes. A path is in
the working copy if and only if it falls under at least one listed prefix.
Below limitations motivate this proposal:

- There is no way to say "everything except `build/`."
  Users who want that must enumerate every sibling directory by hand and
  keep the list updated as the tree changes.
- Patterns are literal directory prefixes
  only; there's no way to select, say, all `*.proto` files across the tree.
- Sparse commands resolve bare paths
  relative to the repository root, while every other command that accepts
  paths in jj (`diff`, `restore`, `commit`, etc.) resolves them relative to
  the current working directory. This is a persistent source of surprise
  for anyone running sparse commands from a subdirectory.
- Output format is a flat path list, which is sufficient for a flat
  prefix model but has no way to represent a richer expression. 

## Prior work

An earlier proposal, **Sparse Patterns v2** (`docs/design/sparse-v2.md`), addressed the exclusion gap differently: it kept sparse
patterns as an ordered list of typed rules (directory / files-only / exact
path, each either include or exclude), evaluated by "first match in reverse
order wins." That design solves exclusion and explicit rule precedence, but
introduces a second, sparse-specific pattern syntax that users have to learn
in addition to the Fileset language they already use for every other
path-taking command. It was never implemented. This proposal takes the
alternative path of reusing the Fileset language directly, trading explicit
rule-ordering semantics for consistency with the rest of the CLI and no new
syntax to learn. 

## Goals and non-goals

### Goals
* Use `jj`'s standard fileset DSL (`FilesetExpression`) to define and evaluate
  sparse checkouts.
* Support directory-relative path parsing across all `jj sparse` subcommands,
  so patterns like `./file` or `../dir` resolve relative to the user's terminal
  location.
* Allow sparse patterns to express exclusion, not just inclusion — directly
  answering the primary ask and follow-up discussion in
  [#7815](https://github.com/jj-vcs/jj/issues/7815).
* Allow sparse patterns to use glob matching, not just directory prefixes.
* Give users a single expression language to learn for "which files am I
  looking at," shared across sparse, diff, restore, and commit.

### Non-goals
* Renaming repository paths to different locations in the
  working folder (e.g. mapping `repo/foo` to `wc/bar`) is out of scope.
* Normalizing complex logical expressions
  (e.g., simplifying `(A & B) | (A & C)` to `A & (B | C)`) is not required for
  correctness and is deferred.

## Proposed Solution
Instead of maintaining a separate domain-specific language, tracking logic, or custom pattern matcher for sparse checkouts, we propose **re-using `jj`'s Fileset engine to define the sparse working copy state**. 

Under this design, a user's sparse configuration is represented globally as a single, canonicalized **Fileset expression** stored within the operation log. The files materialized in the physical working copy match exactly the set of files that evaluate to true under that expression.

## Detailed Design

### 1. Storage & Schema Migration

We introduce a flat string `fileset_expression` field to the relevant Protobuf
structure (`SparsePatterns`) that stores the **path-resolved, canonicalized
string representation** of the fileset AST. The legacy `prefixes` list is
preserved to maintain compatibility:

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

### 2. DSL Syntax & Serialization

To save the sparse state to disk and display it to the user in a consistent
way, the `FilesetExpression` AST is serialized back to a canonical DSL format
using root-relative path markers:

```
[User Input/CLI] ──> [Parsed AST] ──> [Canonicalized/Parenthesized String] ──> [Stored in Proto]
```

* Exact file matches are formatted as `root:"path/to/file.rs"`
* Directory matches are formatted as `root:"path/to/dir"`
* Glob patterns are formatted as `root-glob:"src/**/*.rs"` or
  `root-prefix-glob:"cli/src/commands/*.rs"`

### 3. User Experience (CLI)

All commands interpret and evaluate file and path arguments relative to the user's active shell directory.

#### Command semantics

Let `current` denote whatever expression is presently associated with the
working copy.

- **`jj sparse set <FILESETS>...`**: replaces `current` with the union of
  the given expressions. This is a full replacement, equivalent to today's
  "clear, then add."
- **`jj sparse set --add <FILESETS>`**: sets the working copy to
  `current` unioned with the given expression: everything that was visible
  before, plus the new expression.
- **`jj sparse set --remove <FILESETS>`**:  sets the working copy to
  `current` minus the given expression: everything that was visible before,
  except what matches the new expression.
- **`--add` and `--remove` may be combined** in a single invocation, with
  removal taking precedence over addition for any overlap — consistent
  with how union and difference already compose in the Fileset language.
- **`jj sparse reset`**: restores the working copy to "everything,"
  unchanged in meaning from today.
- **`jj sparse list`**: deprecated in favor of jj sparse show.  
- **`jj sparse show`**: displays the current sparse
  expression in canonical Fileset syntax (e.g. `root:"src" | root:"docs"`,
  or `all() ~ root:"build"` for an exclusion), rather than a flat path list.
- **`jj sparse edit`**:  opens the current expression, as a single piece of
  text, in the user's editor; on save it's re-parsed as one expression. A
  malformed expression is rejected with a clear error before anything about
  the working copy is changed.

#### Pattern Compounding Examples

##### Example 1: Multi-Flag Grouping (Single Command)
When `--add` and `--remove` flags are mixed in one command, additions are
unioned (`|`) first, and removals are subtracted (`~`) as a group:
* **Initial State:** `root:"src"`
* **Command:** `jj sparse set --add doc --add README.md --remove src/temp --remove src/legacy`
* **Resulting Fileset:** `(root:"src" | root:"doc" | root:"README.md") ~ (root:"src/temp" | root:"src/legacy")`
*(Parentheses are automatically added around the groups to preserve correct evaluation order).*

##### Example 2: Operator Accumulation (Consecutive Commands)
Separate commands build rules incrementally. Parentheses are injected automatically
to preserve the logical evaluation order:
1. **Exclude a folder:**
   * `jj sparse set --remove src/temp`
   * **Result:** `all() ~ root:"src/temp"`
2. **Add a folder (Union is appended):**
   * `jj sparse set --add doc`
   * **Result:** `all() ~ root:"src/temp" | root:"doc"` *(No parentheses needed; Difference `~` naturally binds tighter than Union `|`)*
3. **Exclude a nested folder (Subtracted from the whole expression):**
   * `jj sparse set --remove doc/temp`
   * **Result:** `(all() ~ root:"src/temp" | root:"doc") ~ root:"doc/temp"` *(Parentheses added to protect the lower-precedence Union)*

### 4. Working Copy Materialization

During checkouts, updates, or merges, the working copy uses the active
`FilesetExpression` to construct an optimized path matcher. This matcher
filters the tree difference stream, ensuring only matching files are added,
modified, or removed on disk.

## Compatibility & Migration

### Backward Compatibility
Repositories created before this change will not have the new fileset
expression string populated in their Protobuf data store.
* **Fallback Behavior:** If the fileset expression is empty, the system
  checks for legacy prefix lists. If found, it automatically translates them
  into a collection of root-relative prefix matches combined via Union (e.g.,
  `dir1/ | dir2/`) to provide an instantaneous, zero-cost upgrade path.
* If neither field is present, the repository defaults to a full checkout
  (`all()`).

  TODO: Expand this section to follow guidline on deprecation timelines/backwards guarantees.

## Alternatives considered

### 1. Structured Protobuf AST vs. Flat DSL String
* **Pros of Structured Proto AST:** It provides resilient backward compatibility if internal fileset string keywords or syntax semantics change in future major versions of `jj`.
* **Why Rejected It (Pros of Flat String):**
  * **Simplicity:** A flat string dramatically simplifies the data schema and requires no custom Protobuf-to-AST translation layers.
  * **Unified Serialization:** Storing a string means we reuse a single serialization pipeline. If we used a structured Proto AST, we would be forced to maintain *two* distinct serialization implementations: one for `Proto <-> AST` (for internal storage) and another for `AST <-> String` (to support commands like `jj sparse show` or interactive editing via `jj edit`). Leveraging a single, canonical string representation fulfills both needs cleanly. 

## Issues addressed

- [#7815](https://github.com/jj-vcs/jj/issues/7815) — request to use
  filesets (globs, exclusion) for sparse checkouts. This is the primary
  driver for this proposal.

## Future Possibilities

- **Sparse aliases.** Since the Fileset language already supports
  user-defined aliases, common sparse configurations (e.g. per-team
  working sets) could be named and reused across `jj sparse set` and
  `jj workspace add`, without any further design change.
