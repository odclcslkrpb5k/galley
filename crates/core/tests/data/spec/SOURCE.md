# Vendored GFM test data

From [github/cmark-gfm](https://github.com/github/cmark-gfm) at commit
`27d942c8b0a62d192f616e5bf3578f4b6a89e180` (2026-09-28), fetched 2026-10-11, unmodified.

| File | Upstream path | Licence |
|---|---|---|
| `spec.txt` | `test/spec.txt`: the GitHub Flavored Markdown Spec, version 0.29 (2019-04-06), 672 examples | [CC-BY-SA 4.0](http://creativecommons.org/licenses/by-sa/4.0/), Copyright (C) 2014-15 John MacFarlane; GFM additions by GitHub |
| `extensions.txt` | `test/extensions.txt`: 30 examples for the GFM extensions | BSD-2-Clause, as cmark-gfm's test files (see `COPYING-cmark-gfm`) |

`COPYING-cmark-gfm` is cmark-gfm's licence file, which states both licences. These files are
test data only; they are not part of the built program.

Used by `crates/core/tests/spec.rs` as fidelity oracles (docs/plan.md §9). Known, intended
deviations from the expected HTML are listed, with reasons, in `deviations.txt`.
