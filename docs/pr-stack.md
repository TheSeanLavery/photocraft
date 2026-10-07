# PhotoCraft PR stack

The integration branch `codex/photocraft-integration` combines Sean Lavery’s open
PhotoCraft work: upstream PR #354 (cursor/input), #424 (native release size), and
#434 (rendering modes/recovery), preserving their commit history and current main.

The integration PR targets `storytold/photocraft:main`. Its source branch lives in
`TheSeanLavery/photocraft`. Sean has no upstream branch-write permission, so future
stacked PRs are opened in that fork, where GitHub can target the preceding branch.

The current tip is [`codex/disk-backed-undo`](https://github.com/TheSeanLavery/photocraft/pull/1),
stacked on the integration branch. Start the next change from
`fork/codex/disk-backed-undo` and target that branch in its fork PR.

For new work:

1. Fetch the fork and branch from the latest stack tip, currently
   `fork/codex/disk-backed-undo`, using a new `codex/<topic>` branch.
2. Open the PR in `TheSeanLavery/photocraft` with the preceding stack branch as
   its base. Include links to the predecessor and the upstream integration PR.
3. Start the next change from that new branch and target it in the next PR.
4. Preserve parent history. Merge updated parent branches into descendants;
   do not squash or rewrite shared stack branches while descendants depend on them.
5. After the integration lands upstream, merge updated upstream main into the
   stack, then open/retarget the next landing PR against upstream main. Fork PRs
   cannot be retargeted across repositories: create an upstream PR for that step.

Keep the original upstream PRs open until the combined PR is accepted. They
remain independently reviewable; creating this stack does not merge upstream main.
