# CI workflow template

`simulator-gates.yml` is the GitHub Actions template for the R10 delivery gates.
It is kept here instead of `.github/workflows/` because the current push
credential is a GitHub OAuth app without the `workflow` scope; pushing a workflow
file is rejected by GitHub even when the change is a pure template.

To enable hosted CI:

1. Grant the credential the `workflow` scope (`gh auth refresh -s workflow`) or
   use an SSH deploy key with write access.
2. Copy this file to `.github/workflows/simulator-gates.yml` and push.
3. Keep `tools/gates.sh` as the source of truth; the workflow only invokes the
   same tiers (`quick`, `cross`, weekly `long`).

Until then the gates are local-only: `./tools/gates.sh quick|cross|long|rc`.
