# PRD – Containerized Task Workers for `codex-tasks`

## 1. Overview
Introduce first-class support for running `codex-tasks` worker processes inside isolated container environments (e.g., Docker or Podman). Today each task worker runs directly on the host, inheriting the operator’s filesystem and dependency graph. Containerization enables stronger isolation, reproducibility, and easier onboarding for teams that need consistent toolchains across machines.

This PRD captures the requirements, user journeys, constraints, and open questions needed to scope implementation work.

## 2. Goals
- Allow an operator to opt into launching task workers inside containers while keeping the CLI workflow (`codex-tasks start/send/...`) unchanged for downstream consumers.
- Provide flexible configuration for container image selection, runtime, mounted volumes, environment variables, and resource limits.
- Preserve existing task lifecycle semantics (creation, logging, shutdown, archival) regardless of whether the worker runs on the host or in a container.
- Minimize additional friction for local developers who do not need container isolation.

## 3. Non-goals
- Replace the default host-based worker execution. Container mode should be opt-in per task or via configuration.
- Implement a container registry, image build tooling, or orchestrator (e.g., Kubernetes). Focus on single-node runtimes supported by the local environment (Docker Engine, Docker Desktop, Podman).
- Provide full sandbox guarantees (e.g., secure multi-tenancy). The intent is improved isolation and reproducibility, not a hardened security boundary.

## 4. User stories
- As a platform engineer managing shared runners, I want tasks to execute inside predefined container images so we can control dependencies and reduce host pollution.
- As a developer collaborating on a project with specialized toolchains, I want to start a task with a custom image so teammates get identical environments.
- As an operator running experiments, I want the ability to persist project directories into the container so task results remain accessible after the container exits.
- As a security-conscious user, I want the task to run with constrained CPU/memory and limited network access, configured via CLI flags.

## 5. Functional requirements
1. **Opt-in flag(s)**
   - Extend `codex-tasks start` with new options (tentative names):
     - `--container-image <IMAGE>` (required to enable container mode).
     - `--container-runtime <docker|podman>` (optional, defaults to configured preference).
     - `--container-name <NAME>` (optional, auto-generated when omitted).
     - `--container-env KEY=VALUE` (repeatable) to pass environment variables into the worker.
     - `--container-volume <HOST:CONT:MODE>` (repeatable) to mount directories or files.
     - `--container-workdir <PATH>` override for container working directory.
     - `--container-cpu <QUOTA>` and `--container-memory <LIMIT>` resource hints (runtime-dependent semantics).
   - Provide equivalent configuration keys in `~/.codex/config.toml` to set defaults.

2. **Worker launch flow**
   - When container mode is selected, `codex-tasks start` should:
     1. Resolve task directory (`~/.codex/tasks/<task_id>`).
     2. Construct a container run command that mounts required host paths:
        - Task directory mounted read-write so logs and control pipes remain host-accessible.
        - Optional mounts for project repo or user-specified paths.
     3. Launch the container with the same worker binary/entrypoint used today (e.g., `codex-task worker`).
     4. Capture the container ID and store it alongside existing task metadata (e.g., within `task.json`).
   - Ensure the parent process still records PID information or equivalent for status checks.

3. **Lifecycle management**
   - `codex-tasks stop` must gracefully stop containers:
     - Send shutdown signal inside container.
     - On timeout, issue `docker kill`/`podman kill`.
   - `codex-tasks status` should surface container-related details (runtime, image, state) and mark tasks as `DIED` if the container exits unexpectedly.
   - `codex-tasks archive` needs to clean up container metadata and ensure volumes/logs are persisted locally before removal.

4. **Observability**
   - Logs must remain accessible via `codex-tasks log`; container stdout/stderr should be redirected into existing log files.
   - Provide hooks for retrieving container diagnostics (e.g., expose `task.container_id`).

5. **Error handling**
   - Detect missing runtimes (Docker daemon down, binary not installed) and produce actionable error messages before task creation.
   - Fail fast on invalid image references or permission denials, surfacing suggestions (e.g., run `docker login`).

6. **Compatibility**
   - Container mode must co-exist with other CLI enhancements (e.g., flag pass-through to `codex start` such as `--cd`, `-m`).
   - Ensure the feature works on Linux and macOS where Docker/Podman are available; Windows support (via WSL2/Docker Desktop) is a stretch goal.

## 6. User experience & configuration
- Document new flags in `codex-tasks start --help` and provide examples in README/docs.
- Support configuration precedence: CLI flags override task profile overrides, which override global config defaults.
- Consider shorthand presets (e.g., `--container-preset <name>`) referencing config snippets for teams.
- Provide dry-run mode (`--container-dry-run`) to output the container command without executing, aiding debugging.

## 7. Technical considerations
- **Runtime abstraction**: Implement a small wrapper module to translate generic container options into runtime-specific commands (`docker run`, `podman run`).
- **Task IPC**: Maintain named pipe / log files on the host by mounting the task directory; confirm FIFO semantics survive bind mounts.
- **Process supervision**: Capture container ID and rely on runtime CLI or API for status checks; avoid long-lived background daemons.
- **Permissions**: Honor rootless Docker/Podman setups; detect when elevated privileges are required and provide guidance.
- **Image distribution**: Assume images are pre-built and available locally or in a configured registry. Provide optional `--container-pull` flag to force-update.

## 8. Security & isolation
- Highlight that containerization improves process separation but is not a full sandbox; document best practices (minimal base images, read-only mounts, non-root users).
- Allow disabling host network (`--container-disable-network`) or whitelisting specific ports.
- Encourage storing secrets via runtime-specific mechanisms (e.g., Docker secrets) rather than embedding in CLI flags.

## 9. Performance & resource management
- Containers incur startup overhead; capture expected impact and recommend caching warm images.
- Support configurable resource limits to prevent runaway CPU/memory usage on shared machines.
- Evaluate log I/O performance when writing through bind mounts; may require `async` flushing.

## 10. Migration plan
1. **Phase 1 – Foundations**
   - Implement runtime detection, flag parsing, and container launch for Linux.
   - Document feature as experimental.
2. **Phase 2 – Cross-platform & polish**
   - Add macOS/Podman validation, dry-run support, resource limit handling.
   - Expose status/stop integration with container IDs.
3. **Phase 3 – Advanced features**
   - Preset support, network controls, integration testing.

## 11. Risks & mitigations
- **Runtime availability**: Users may lack Docker/Podman. Mitigation: preflight checks with explicit errors and fallback to host mode.
- **Permission issues**: Rootless environments or corporate policies may block container operations. Mitigation: provide documentation and detect unsupported setups early.
- **Shared state leakage**: Misconfigured mounts could expose host data. Mitigation: provide safe defaults (only task directory) and warnings when mounting `/` or home directories.
- **Complex UX**: Too many flags may overwhelm users. Mitigation: offer presets and config profiles.

## 12. Open questions
- Should container images ship with pre-installed Codex CLI, or should the worker binary be mounted in? How to version-sync the host CLI with container contents?
- Do we support GPU passthrough and, if so, what extra flags are required (e.g., `--gpus`)?
- How do we surface container logs beyond the worker transcript (e.g., runtime events)?
- What is the policy for cleaning up stopped containers and dangling volumes? Should we offer automatic pruning?
- How does container mode interact with future remote/external worker orchestration plans?

## 13. Success metrics
- Percentage of tasks successfully launched in container mode without manual container commands.
- Time-to-first-task (container) within 20% of host mode for warm images.
- Reduction in environment-specific task failures reported by users adopting container mode.

## 14. Documentation updates
- Update `README.md` and `docs/prd.md` to reference the new capability once implemented.
- Add troubleshooting section covering common container errors and workarounds.

## 15. Appendices
- **References**: Docker CLI docs (v26, September 2025), Podman 5.x docs, existing `docs/prd.md` future enhancements list.
- **Glossary**: Container runtime, bind mount, image, preset.

