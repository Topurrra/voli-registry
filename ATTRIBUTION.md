# Attribution

## Scoop `main` bucket

A large share of the manifests under `manifests/` are automatically converted
from the [Scoop `main` bucket](https://github.com/ScoopInstaller/Main)
(`ScoopInstaller/Main`), which is distributed under the **MIT License**.

The conversion is mechanical (Scoop JSON → Voli TOML; see `tools/`). Package
metadata — names, versions, download URLs, hashes, and bin lists — originates
upstream. We retain the upstream MIT license and this attribution as required.

Manifests with `pre_install` / `post_install` / `installer` scripts are **not**
imported (Voli's no-script rule), so the imported subset is a script-free
portion of the upstream bucket.

Upstream license: <https://github.com/ScoopInstaller/Main/blob/master/LICENSE>

## This repository

Everything else in this repository (the layout, CI, and Voli-authored
manifests) is MIT-licensed — see `LICENSE`.
