# `main` Branch Protection

Last updated: September 23, 2025

The `main` branch now enforces branch protection to keep production-ready code safe.

## Requirements

- **Pull requests only** – at least one approving review is required before merging.
- **Status checks** – the GitHub Actions job `test` must pass on the latest commit.
- **Strict updates** – branches must be up to date with `main` before merge.
- **Admin enforcement** – administrators must also respect the rules.
- **No force pushes or deletions** – direct history rewrites are blocked.
- **Linear history & conversations** – rebases (no merge commits) are required and all review threads must be resolved.

## Maintenance

You can inspect the current rules with:

```bash
gh api repos/pnyczyk/codex-tasks/branches/main/protection
```

To adjust the configuration, edit the JSON payload in `.github/branch-protection.json` (create if needed) and apply it with:

```bash
gh api --method PUT \
  repos/pnyczyk/codex-tasks/branches/main/protection \
  --header 'Accept: application/vnd.github+json' \
  --input .github/branch-protection.json
```

Keep this document in sync with any future changes.
