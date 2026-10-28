# Skill import report

Generated 327 deterministic skill archives from 10 pinned sources.

| Source | Revision | Skills | License |
| --- | --- | ---: | --- |
| `google/agents-cli` | `f4e994044164ff6617f0799900dba0fc091ef0e3` | 7 | Apache-2.0 |
| `huggingface/skills` | `fabc634a893f62ad5c2039830397efa35fa62715` | 26 | Apache-2.0 |
| `android/skills` | `45fd1739a4dbca5452c3e936570797f7f6261cd7` | 36 | Apache-2.0 |
| `dotnet/skills` | `cf03b2d2ee17ad2800763108698d869f5b997774` | 106 | MIT |
| `anthropics/skills` | `dbd4588f9e1033efb41dad4bef2f7947c8993d44` | 14 | Apache-2.0 |
| `obra/superpowers` | `2ce8aa15d05bd57213e6f746f953aafb7ded7a8b` | 15 | MIT |
| `mattpocock/skills` | `59db609eb1e18aaf59b63284d0e52255aff0c63c` | 31 | MIT |
| `emilkowalski/skills` | `2d228630ba1446365f614a108cb8252f1feda202` | 15 | MIT |
| `MiniMax-AI/skills` | `60aaae52bb2af8162732751a4332f62a5fef518b` | 17 | MIT |
| `davidondrej/skills` | `9170142cfa1a57e2609d42fc7598a4788a8adbd7` | 60 | MIT |

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
