# Skill import report

Generated 319 deterministic skill archives from 10 pinned sources.

| Source | Revision | Skills | License |
| --- | --- | ---: | --- |
| `google/agents-cli` | `d445570c61fd446487ea6623c5635b23aded237f` | 7 | Apache-2.0 |
| `huggingface/skills` | `c3ff942be3e07116b475253f0a8fc4f54682c1a8` | 26 | Apache-2.0 |
| `android/skills` | `ed0a2728ff531174383f910c649694de1eaebcd6` | 30 | Apache-2.0 |
| `dotnet/skills` | `9386ed7c1148a7bef970aa0502371c982effa01c` | 105 | MIT |
| `anthropics/skills` | `dbd4588f9e1033efb41dad4bef2f7947c8993d44` | 14 | Apache-2.0 |
| `obra/superpowers` | `bb92a77741419a4ab5f06e711a283343f1ada0c3` | 15 | MIT |
| `mattpocock/skills` | `5beb35b040e3de97e54fa9e386d7e42d15c295c2` | 31 | MIT |
| `emilkowalski/skills` | `2d228630ba1446365f614a108cb8252f1feda202` | 15 | MIT |
| `MiniMax-AI/skills` | `60aaae52bb2af8162732751a4332f62a5fef518b` | 17 | MIT |
| `davidondrej/skills` | `979beb7077585445b13139f3182551cc30434749` | 59 | MIT |

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
