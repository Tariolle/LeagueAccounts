# GitHub repository boundaries

This checkout is the fork `fallenyt9-eng/LeagueAccounts`. Its upstream parent is `Tariolle/LeagueAccounts`.

## Before GitHub writes

- Before pushing, opening or merging a pull request, tagging, or publishing a release, verify the current repository with `git remote -v` and `gh repo view fallenyt9-eng/LeagueAccounts --json nameWithOwner,isFork,parent`.
- Treat `fallenyt9-eng/LeagueAccounts` as the only write target by default. The upstream repository `Tariolle/LeagueAccounts` is read-only unless the user explicitly asks to contribute to upstream.
- Never infer the pull request base repository from a branch name or the GitHub UI default. Explicitly create fork pull requests with `gh pr create --repo fallenyt9-eng/LeagueAccounts --base main --head <branch>`.
- After creating a pull request, verify its REST metadata. Require `.base.repo.full_name == "fallenyt9-eng/LeagueAccounts"` and `.base.ref == "main"`; if either differs, stop and report the mismatch. The head may also be a branch in this fork.
- Create tags and releases only in `fallenyt9-eng/LeagueAccounts`, unless the user explicitly requests upstream publication.
- Do not merge a pull request unless the user explicitly requests the merge. When requested, verify again that its base is this fork's `main` and that required checks passed.

## Pull request verification

For PR number `<number>`, verify the destination with:

```sh
gh api repos/fallenyt9-eng/LeagueAccounts/pulls/<number> --jq '{base:.base.repo.full_name,base_branch:.base.ref,head:.head.repo.full_name,head_branch:.head.ref}'
```

Do not treat a successful PR creation response as proof of the destination; inspect the PR's base repository and branch.
