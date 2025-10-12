# Coding Agent Workflow

1. **Repository access**
   - All work lives in the GitHub repository (`pnyczyk/codex-tasks`).
   - The agent interacts with GitHub via the `gh` CLI for issues, branches, and pull requests.

2. **Task intake**
   - When the user provides a new request, first search existing GitHub issues to see if the scope already overlaps.
     - If an overlapping issue exists, stop and propose re-scoping or reusing that ticket instead of duplicating effort.
   - If no existing issue covers the request, create a fresh GitHub issue describing the task.
     - Capture multiline Markdown in a file and pass it with `gh issue create --body-file <path>` (e.g. use `cat <<'EOF' >/tmp/issue.md` … `EOF`) instead of embedding the content directly on the command line. This avoids broken formatting from shell-escaped backticks, angle brackets, or newlines.
     - When a lightweight description is sufficient and you stay on the command line, wrap inline code with backticks **after** quoting the string (e.g. `--body "Run \`cargo test\`"`) so the shell does not strip Markdown characters.
   - The above does not apply to non-coding tasks like reviews, questions regarding code, etc.

3. **Branching strategy**
   - Create a feature branch using the pattern `feat/<issue-number>-<short-name>`.
   - All implementation, tests, and documentation for the task happen on that branch.

4. **Implementation & testing**
   - Implement the requested changes, keeping commits scoped to the new functionality.
   - Run relevant test suites or scripts to ensure the change works end-to-end.

5. **Pull request**
   - Once tests pass and the work is ready, open a pull request from the feature branch back to `main`.
   - Ensure the PR references the GitHub issue created in step 2 and includes a summary of changes and testing notes.
