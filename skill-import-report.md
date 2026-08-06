# Skill import report

Generated 271 deterministic skill archives from 10 pinned sources.

| Source | Revision | Skills | License |
| --- | --- | ---: | --- |
| `google/agents-cli` | `c7a375f7a463d5ade51caabdec56971681aed400` | 7 | Apache-2.0 |
| `huggingface/skills` | `32f8bb0928e95fc9d47ca9fbf69cbfbaf2bc2bda` | 26 | Apache-2.0 |
| `android/skills` | `ba0042c08b7e6ff5cb121b7b87d442f809467324` | 20 | Apache-2.0 |
| `dotnet/skills` | `6fce087f5e72ce493ee1d44ceb0ecce6acc1e4dc` | 96 | MIT |
| `anthropics/skills` | `b29e7cf65e5cb78a5ac33d582270551bc74a14eb` | 12 | Apache-2.0 |
| `obra/superpowers` | `44c9b2d6e889982ac18c27d05a19fefe335194e1` | 14 | MIT |
| `mattpocock/skills` | `2ab958093e83e0ec752e6c1c5932da465bf23e0c` | 28 | MIT |
| `emilkowalski/skills` | `da80201b64de7d608a6dc5f723797ce6c65b692b` | 8 | MIT |
| `MiniMax-AI/skills` | `60aaae52bb2af8162732751a4332f62a5fef518b` | 17 | MIT |
| `davidondrej/skills` | `6e5545081c888b89576a620d9b2e54e9a6590f68` | 43 | MIT |

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
