# Skill import report

Generated 309 deterministic skill archives from 10 pinned sources.

| Source | Revision | Skills | License |
| --- | --- | ---: | --- |
| `google/agents-cli` | `d445570c61fd446487ea6623c5635b23aded237f` | 7 | Apache-2.0 |
| `huggingface/skills` | `c3ff942be3e07116b475253f0a8fc4f54682c1a8` | 26 | Apache-2.0 |
| `android/skills` | `2ee8c61ff64f29a19213f116807c79740749dccf` | 25 | Apache-2.0 |
| `dotnet/skills` | `3b4ec85cfe6dddc62564db6783593ca1cc293cd0` | 104 | MIT |
| `anthropics/skills` | `dbd4588f9e1033efb41dad4bef2f7947c8993d44` | 14 | Apache-2.0 |
| `obra/superpowers` | `bb92a77741419a4ab5f06e711a283343f1ada0c3` | 15 | MIT |
| `mattpocock/skills` | `49dd158d1076134a641b33efb035946536778336` | 31 | MIT |
| `emilkowalski/skills` | `2d228630ba1446365f614a108cb8252f1feda202` | 15 | MIT |
| `MiniMax-AI/skills` | `60aaae52bb2af8162732751a4332f62a5fef518b` | 17 | MIT |
| `davidondrej/skills` | `e5754e9b8c27026ccb6bac35aa4423171d3c7871` | 55 | MIT |

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
