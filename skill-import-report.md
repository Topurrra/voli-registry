# Skill import report

Generated 304 deterministic skill archives from 10 pinned sources.

| Source | Revision | Skills | License |
| --- | --- | ---: | --- |
| `google/agents-cli` | `2c3945901e8e5cc933fa94fa44d1d4aaf644b596` | 7 | Apache-2.0 |
| `huggingface/skills` | `ca0325bb20b2d0a1b2efa893670c4c72f79e707b` | 26 | Apache-2.0 |
| `android/skills` | `42dc2270e96032bd860bb94511e440aa00a43125` | 25 | Apache-2.0 |
| `dotnet/skills` | `0dcd43ceb15e15a2c45dd54075fbcf3a30632154` | 100 | MIT |
| `anthropics/skills` | `683bc88e56f3e09ba94f7055977f3d3aa499f202` | 14 | Apache-2.0 |
| `obra/superpowers` | `8ca22dba9a94f28898bbce59f2537ff4d87c747d` | 15 | MIT |
| `mattpocock/skills` | `c665c5559e8be56a12271a620ae44aa1ada535ed` | 31 | MIT |
| `emilkowalski/skills` | `e8a175de22ae1e49370fc144c1f3bb9aeedf988d` | 14 | MIT |
| `MiniMax-AI/skills` | `60aaae52bb2af8162732751a4332f62a5fef518b` | 17 | MIT |
| `davidondrej/skills` | `f025cb43cbbfe5810b130a207c4353c8555af7cb` | 55 | MIT |

## Name collisions

Two or more sources shipped the same skill name. The bare name stays with
the first source in `skill-sources.toml` order; every later claimant is
published under `<prefix>-<name>`.

Renaming is not metadata-only: the client requires the manifest name, the
archive's top-level directory, and the `name:` field in the archived
`SKILL.md` to agree. So for each row below the importer rewrites that one
frontmatter field inside the archive. Every other byte of upstream content
is copied verbatim.

| Upstream name | Published as | Source | Path |
| --- | --- | --- | --- |
| `prototype` | `emilkowalski-prototype` | `emilkowalski/skills` | `skills/prototype` |

Release publishing and PR merging are deferred.
