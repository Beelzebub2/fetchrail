# Repository instructions

## Branch names

Name branches for the work they contain, using `<type>/<short-kebab-case-description>`:

- `fix/` for bug fixes and regressions.
- `feat/` for new features or capabilities.
- `perf/` for performance optimizations.
- `docs/` for documentation and repository guidance.
- `refactor/` for restructuring without changing behavior.
- `test/` for test-only changes.
- `ci/` for continuous integration workflows.
- `chore/` for maintenance, tooling, or dependency updates.

Choose the prefix that describes the primary purpose of mixed changes. Do not use
agent names or generic prefixes such as `codex/`. Keep `main` as the integration
branch. Examples: `fix/private-browser-downloads`, `feat/download-queues`, and
`perf/torrent-inspector`.
