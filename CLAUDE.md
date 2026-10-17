## Agent skills

### Issue tracker

Issues are tracked in GitHub Issues on SuplenC/ukiyo (via `gh`). See `docs/agents/issue-tracker.md`.

### Triage labels

Default vocabulary: needs-triage, needs-info, ready-for-agent, ready-for-human, wontfix. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `GLOSSARY.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.

## Git

### Commits

Follow [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/): `<type>(<scope>): <summary>`.

- **Types**: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`.
- **Scope**: optional. Use the crate name without the `ukiyo-` prefix (`core`, `preview`, `activityd`), or `ukiyo` for the daemon and CLI crate. Name a top-level area (`scenes`, `nix`) when the change sits outside the crates.
- **Summary**: imperative mood, lowercase, no trailing period, at most 72 characters.
- **Breaking changes**: put `!` after the type or scope, and explain the change in a `BREAKING CHANGE:` footer.
- **Issues**: reference them in a footer, e.g. `Closes #12`.

### Branches

Follow [Conventional Branches](https://conventional-branch.github.io/): `<type>/<description>`.

- **Types**: `feature/`, `bugfix/`, `hotfix/`, `release/`, `chore/`. `main` is the trunk.
- **Description**: lowercase letters, digits and single hyphens. Dots are allowed only in release versions (`release/v1.2.0`). Start the description with the issue number when there is one: `feature/issue-12-audio-reactive-scenes`.
