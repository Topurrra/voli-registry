# Skill import report

Generated 334 deterministic skill archives from 10 pinned sources.

| Source | Revision | Skills | License |
| --- | --- | ---: | --- |
| `google/agents-cli` | `896e4719c4184841325a301755781d6b3da233ef` | 7 | Apache-2.0 |
| `huggingface/skills` | `25910b3d0ba31a2ba102f901cdc9e008c278a69c` | 26 | Apache-2.0 |
| `android/skills` | `5815d27fda564aa9f07ebdcb5e57ed3e8d904e0d` | 36 | Apache-2.0 |
| `dotnet/skills` | `96c3d68dad2855e3ab5178aa1a262789d86a1466` | 108 | MIT |
| `anthropics/skills` | `2c5958ecac7cee2b583967a2c4981d0f4a75c579` | 14 | Apache-2.0 |
| `obra/superpowers` | `22f0b463e7a5e579904dd6211a520321035526f7` | 15 | MIT |
| `mattpocock/skills` | `4ae2317ddb22b046e26455e4f9c9b3aaaf62212d` | 31 | MIT |
| `emilkowalski/skills` | `715c1807e08c497cfd1dc4feb450a2a3300ef2a9` | 18 | MIT |
| `MiniMax-AI/skills` | `60aaae52bb2af8162732751a4332f62a5fef518b` | 17 | MIT |
| `davidondrej/skills` | `cf0b20e4254fbeca1d4535a32cd735ecc49ec325` | 62 | MIT |

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
| `handoff` | `davidondrej-handoff` | `davidondrej/skills` | `skills/handoff` |
| `teach` | `davidondrej-teach` | `davidondrej/skills` | `skills/teach` |
| `prototype` | `emilkowalski-prototype` | `emilkowalski/skills` | `skills/prototype` |

Release publishing and PR merging are deferred.
