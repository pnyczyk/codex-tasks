# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.1] - 2025-09-27
### Added
- `archive -a/--all` bulk-archives every task currently in `STOPPED` or `DIED` state.

### Changed
- `archive` now skips running/idle tasks when using `-a` and reports skipped/archived tasks in the CLI output.

## [0.3.0] - 2025-09-27
### Added
- `log -f` now exits automatically once the worker returns to `IDLE`, `STOPPED`, or `DIED`.
- `--forever`/`-F` flag retains the original tail-forever behavior and implies `--follow`.
- CLI documentation and PRD updates describing the new log-follow semantics.

### Changed
- `log -f` no longer blocks indefinitely after the worker finishes unless `--forever` is provided.

## [0.2.0] - 2025-09-26
### Added
- `codex start` options for custom Codex config files, working directories, and repository cloning.
- Worker support for honoring the new CLI flags when launching `codex proto`.
- Integration tests and documentation for the enhanced start workflow.
