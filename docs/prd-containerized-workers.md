# PRD – Containerized & Provider-Agnostic Task Workers for `codex-tasks`

## 1. Overview
Introduce first-class support for running `codex-tasks` worker processes inside isolated container environments (e.g., Docker or Podman) while laying the groundwork for multiple AI provider CLIs. Today each task worker runs directly on the host, inherits the operator’s filesystem, and assumes the Codex CLI. Containerization plus a provider abstraction enables stronger isolation, reproducibility, and future compatibility with CLIs such as Claude Code and Gemini without rewriting the task lifecycle.

This PRD captures the requirements, user journeys, constraints, and open questions needed to scope implementation work. The first delivery targets Codex-only workers, but all architecture and UX decisions must safely extend to additional providers.

## 2. Goals
- Allow an operator to opt into launching task workers inside containers while keeping the user-facing workflow (`codex-tasks start/send/...`) unchanged for downstream consumers.
- Provide flexible configuration for container image selection, runtime, mounted volumes, environment variables, and resource limits.
- Preserve existing task lifecycle semantics (creation, logging, shutdown, archival) regardless of whether the worker runs on the host or inside a container.
- Establish a provider-agnostic execution surface so Codex, Claude Code, Gemini CLI, or others can plug into the same task orchestration in the future.
- Minimize additional friction for local developers who do not need container isolation.

## 3. Non-goals
- Replace the default host-based worker execution. Container mode remains opt-in per task or via configuration.
- Ship first-class support for non-Codex providers in the initial release. Instead, design the system so subsequent providers are additive changes, not refactors.
- Implement a container registry, image build tooling, or orchestrator (e.g., Kubernetes). Focus on single-node runtimes supported by the local environment (Docker Engine, Docker Desktop, Podman).
- Provide full sandbox guarantees (e.g., secure multi-tenancy). The intent is improved isolation and reproducibility, not a hardened security boundary.

## 4. User stories
- As a platform engineer managing shared runners, I want tasks to execute inside predefined container images so we can control dependencies and reduce host pollution.
- As a developer collaborating on a project with specialized toolchains, I want to start a task with a custom image so teammates get identical environments.
- As an operator running experiments, I want the ability to persist project directories into the container so task results remain accessible after the container exits.
- As a security-conscious user, I want the task to run with constrained CPU/memory and limited network access, configured via CLI flags.
- As a team lead evaluating alternative AI providers, I want to configure task workers to launch with either the Codex CLI today or other CLIs (Claude Code, Gemini) once available, without changing downstream task management commands.

## 5. Functional requirements
1. **Provider abstraction**
   - Introduce a provider selection surface (tentative: `--provider <codex|claude|gemini|...>`). Default to `codex` for backward compatibility.
   - Allow provider-specific argument passthrough (tentative: `--provider-arg KEY=VALUE` or provider-scoped flag groups). Preserve existing Codex flags such as `--cd`, `-m`, or `-c` by routing them through this mechanism.
   - Persist provider metadata in task records (`task.json`) so status/stop/log commands can react appropriately.
   - Initial implementation must support Codex end-to-end; other providers become follow-on work but should require configuration and image updates rather than CLI redesign.

2. **Opt-in container flags**
   - Extend `codex-tasks start` with new options (tentative names):
     - `--container-image <IMAGE>` (required to enable container mode).
     - `--container-runtime <docker|podman>` (optional, defaults to configured preference).
     - `--container-name <NAME>` (optional, auto-generated when omitted).
     - `--container-env KEY=VALUE` (repeatable) to pass environment variables into the worker.
     - `--container-volume <HOST:CONT:MODE>` (repeatable) to mount directories or files.
     - `--container-workdir <PATH>` override for container working directory.
     - `--container-cpu <QUOTA>` and `--container-memory <LIMIT>` resource hints (runtime-dependent semantics).
   - Provide equivalent configuration keys in `~/.codex/config.toml` to set defaults, including per-provider presets (see §6).

3. **Worker launch flow**
   - When container mode is selected, `codex-tasks start` should:
     1. Resolve task directory (`~/.codex/tasks/<task_id>`).
     2. Construct a container run command that mounts required host paths:
        - Task directory mounted read-write so logs and control pipes remain host-accessible.
        - Optional mounts for project repo or user-specified paths.
     3. Launch the container with the provider-specific worker entrypoint (initially `codex-task worker`). Future providers may require different binaries or wrapper scripts; the launch API must support that variation.
     4. Capture the container ID and store it alongside existing task metadata.
   - Ensure the parent process still records PID (or equivalent) information for status checks even when the worker runs inside a container.

4. **Lifecycle management**
   - `codex-tasks stop` must gracefully stop containers:
     - Send the provider-specific shutdown signal inside the container.
     - On timeout, issue `docker kill`/`podman kill`.
   - `codex-tasks status` should surface provider and container details (runtime, image, container ID/state) and mark tasks as `DIED` if the container exits unexpectedly.
   - `codex-tasks archive` needs to clean up container metadata and ensure volumes/logs persist locally before removal.

5. **Observability**
   - Logs must remain accessible via `codex-tasks log`; container stdout/stderr should be redirected into existing log files regardless of provider.
   - Provide hooks for retrieving container diagnostics (e.g., expose `task.container_id`, `task.provider`).

6. **Error handling**
   - Detect missing runtimes (Docker daemon down, binary not installed) and produce actionable error messages before task creation.
   - Fail fast on invalid image references or permission denials, surfacing suggestions (e.g., run `docker login`).
   - Emit clear errors when a requested provider lacks an installed CLI or configured container image.

7. **Compatibility**
   - Container mode must co-exist with provider argument passthrough and other CLI enhancements.
   - Ensure the feature works on Linux and macOS where Docker/Podman are available; Windows support (via WSL2/Docker Desktop) remains a stretch goal.

## 6. User experience & configuration
- Document new flags in `codex-tasks start --help` and provide examples in README/docs, including how provider and container options interact.
- Support configuration precedence: CLI flags override task profile overrides, which override global config defaults. Profiles may specify both provider and container settings.
- Consider shorthand presets (e.g., `--container-preset <name>`) referencing config snippets for teams; presets can bundle provider selection, image, and default mounts.
- Provide dry-run mode (`--container-dry-run`) to output the container command and provider invocation without executing, aiding debugging.

## 7. Technical considerations
- **Runtime abstraction**: Implement a wrapper module to translate generic container options into runtime-specific commands (`docker run`, `podman run`) and host future runtimes.
- **Provider plug-in**: Define an interface for provider adapters (command builder, shutdown signal, log parsing quirks). Codex adapter ships first; Claude/Gemini adapters become incremental additions.
- **Task IPC**: Maintain named pipe / log files on the host by mounting the task directory; confirm FIFO semantics survive bind mounts across providers.
- **Process supervision**: Capture container ID and possibly provider-specific process IDs inside the container. Use runtime CLI or API for status checks; avoid long-lived daemons.
- **Permissions**: Honor rootless Docker/Podman setups; detect when elevated privileges are required and provide guidance.
- **Image distribution**: Assume images are pre-built and available locally or in a configured registry. Provide optional `--container-pull` flag to force-update. Document that different providers likely require distinct base images.

## 8. Security & isolation
- Highlight that containerization improves process separation but is not a full sandbox; document best practices (minimal base images, read-only mounts, non-root users).
- Allow disabling host network (`--container-disable-network`) or whitelisting specific ports. Note that some providers may require outbound network calls; communicate requirements per provider.
- Encourage storing secrets via runtime-specific mechanisms (e.g., Docker secrets) rather than embedding in CLI flags.

## 9. Performance & resource management
- Containers incur startup overhead; capture expected impact and recommend caching warm images. Measure provider-specific differences where relevant.
- Support configurable resource limits to prevent runaway CPU/memory usage on shared machines.
- Evaluate log I/O performance when writing through bind mounts; may require `async` flushing.

## 10. Migration plan
1. **Phase 1 – Foundations (Codex-only)**
   - Implement runtime detection, provider abstraction scaffolding (with `codex` as the only supported provider), flag parsing, and container launch for Linux.
   - Document feature as experimental and note that additional providers are planned but not yet available.
2. **Phase 2 – Cross-platform & polish**
   - Add macOS/Podman validation, dry-run support, resource limit handling.
   - Expose status/stop integration with container IDs and provider metadata.
3. **Phase 3 – Provider expansion & advanced features**
   - Add adapters for other CLIs (Claude Code, Gemini) once validated.
   - Introduce presets, network controls, integration testing, and shared documentation updates.

## 11. Risks & mitigations
- **Runtime availability**: Users may lack Docker/Podman. Mitigation: preflight checks with explicit errors and fallback to host mode.
- **Permission issues**: Rootless environments or corporate policies may block container operations. Mitigation: provide documentation and detect unsupported setups early.
- **Shared state leakage**: Misconfigured mounts could expose host data. Mitigation: provide safe defaults (only task directory) and warnings when mounting `/` or home directories.
- **Complex UX**: Too many flags may overwhelm users. Mitigation: offer presets, configuration profiles, and provider defaults.
- **Provider divergence**: Different CLIs may need unique shutdown sequences or logging formats. Mitigation: enforce provider adapter interface and test harness per provider.

## 12. Open questions
- How should container images include provider binaries? Ship images with the CLI baked in, mount host binaries, or install on launch?
- What minimum feature set is required to consider the provider abstraction “done” for Codex? (e.g., parity with classic Codex CLI flags.)
- Do we support GPU passthrough and, if so, what extra flags are required (e.g., `--gpus`)? Does this vary by provider?
- How do we surface container logs beyond the worker transcript (e.g., runtime events) and do we need provider-specific enrichments?
- What is the policy for cleaning up stopped containers and dangling volumes? Should we offer automatic pruning?
- How does container mode interact with future remote/external worker orchestration plans?

## 13. Success metrics
- Percentage of tasks successfully launched in container mode without manual container commands.
- Time-to-first-task (container) within 20% of host mode for warm images.
- Reduction in environment-specific task failures reported by users adopting container mode.
- Lead time to onboard a new provider CLI measured in days rather than weeks once adapters are in place.

## 14. Documentation updates
- Update `README.md` and `docs/prd.md` to reference the new capability once implemented.
- Add troubleshooting sections covering common container errors, provider misconfiguration, and workarounds.

## 15. Appendices
- **References**: Docker CLI docs (v26, September 2025), Podman 5.x docs, provider CLI docs (Codex, Claude Code, Gemini), existing `docs/prd.md` future enhancements list.
- **Glossary**: Container runtime, provider adapter, bind mount, image, preset.

