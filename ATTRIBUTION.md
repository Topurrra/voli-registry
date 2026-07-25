# Attribution

## Scoop Main and Extras buckets

A large share of the manifests under `manifests/` are automatically converted
from Scoop's [Main](https://github.com/ScoopInstaller/Main) and
[Extras](https://github.com/ScoopInstaller/Extras) buckets. Both are
distributed under the **Unlicense**.

The conversion is mechanical (Scoop JSON → Voli TOML; see `tools/`). Package
metadata, including names, versions, download URLs, hashes, bins, and shortcuts,
originates upstream.

Manifests requiring executable install or uninstall scripts are not imported.
Benign `post_install` and `post_uninstall` metadata is dropped because Voli
expresses persistence declaratively and cannot execute scripts.

Upstream licenses:

- <https://github.com/ScoopInstaller/Main/blob/master/LICENSE>
- <https://github.com/ScoopInstaller/Extras/blob/master/LICENSE>

## This repository

Everything else in this repository (the layout, CI, and Voli-authored
manifests) is MIT-licensed — see `LICENSE`.
