# Workspace Startup Benchmark

Use these `.cnb.yml` variants in a dedicated benchmark repository, not the user's
normal bridge repository. Copy one variant to the benchmark repository's
`.cnb.yml`, commit it, and configure CNB Bridge to use that repository and branch.

Log in to CNB Bridge with an account that can create the benchmark repository
and stop its workspaces, then run:

```powershell
pwsh -File scripts/benchmark-workspace-start.ps1 `
  -RepoSlug "owner/cnb-bridge-benchmark" `
  -Branch "main" `
  -Runs 3 `
  -RepoConfigPath "scripts/benchmark-workspace/current.cnb.yml"
```

The script reads the active encrypted CNB Bridge credential without printing
the token. It creates or verifies the dedicated benchmark repository, publishes
the selected `.cnb.yml`, waits until old workspaces have stopped, and runs the
requested number of fully automated cold starts. Results are written to
`scripts/benchmark-workspace/results/`.

Run all four variants and compare their median `workspace`, `ssh`,
`agent_health`, and total durations. Reject a variant if `remoteSsh` is missing,
SSH cannot forward port 11233, or `/healthz` is unavailable.

Before adopting `node:22-bookworm-slim` in the production template, replace the
moving tag with the resolved image digest.
