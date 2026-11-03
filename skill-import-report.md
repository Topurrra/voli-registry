# Skill import report

Generated 328 deterministic skill archives from 10 pinned sources.

| Source | Revision | Skills | License |
| --- | --- | ---: | --- |
| `google/agents-cli` | `f4e994044164ff6617f0799900dba0fc091ef0e3` | 7 | Apache-2.0 |
| `huggingface/skills` | `25910b3d0ba31a2ba102f901cdc9e008c278a69c` | 26 | Apache-2.0 |
| `android/skills` | `6daa53ef27fa2aa87827d183a85c06cedceca833` | 36 | Apache-2.0 |
| `dotnet/skills` | `95e7d0cb7994d47b5789baa68e13f7a2f348b956` | 106 | MIT |
| `anthropics/skills` | `ca45fc496a3436251f191354091d557da31c15b5` | 14 | Apache-2.0 |
| `obra/superpowers` | `2ce8aa15d05bd57213e6f746f953aafb7ded7a8b` | 15 | MIT |
| `mattpocock/skills` | `4ae2317ddb22b046e26455e4f9c9b3aaaf62212d` | 31 | MIT |
| `emilkowalski/skills` | `f1090bec277140d02ed202440185ad9d02ac1490` | 16 | MIT |
| `MiniMax-AI/skills` | `60aaae52bb2af8162732751a4332f62a5fef518b` | 17 | MIT |
| `davidondrej/skills` | `8aa01cf45110ae378bdf5c319cd41ac8fdbdf448` | 60 | MIT |

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
