# dev-tools

[![Agent IX Plugins](https://github.com/agent-ix/agent-plugins/raw/refs/heads/main/assets/agent-ix-plugins.svg)](https://github.com/agent-ix/agent-plugins)
[![Discord](https://img.shields.io/badge/Discord-Join%20us-5865F2?logo=discord&logoColor=white)](https://discord.gg/k8DVhuYBR2)

Focused review, code guidance, security, backporting, scaffolding, and documentation figure skills.

## Setup

If this plugin is uninitialized or a command fails, follow the [plugin setup guide](setup.md) for its required CLIs, configuration, and local diagnosis.

## Community help

If the setup checks leave a reproducible Agent IX dev-tools bug that blocks progress, [join the Agent IX Discord](https://discord.gg/k8DVhuYBR2). Community help is a last resort for Agent IX product bugs, not a help desk for local credentials, machine setup, third party tools, or unrelated projects. See [setup.md](setup.md#community-help) for what to include.

## Skills

| Skill | Action |
| --- | --- |
| `code-review` | Review code and tests against Agent IX conventions, routing language specific checks. |
| `rust-review` | Review Rust code for idioms, safety, test coverage, and required gates. |
| `review-react` | Review React tests and Storybook coverage against requirements. |
| `rust-style` | Apply documented Rust idioms when a repository has no local style rules. |
| `writing-tests-python` | Write pytest tests using fixture, mocking, and coverage conventions. |
| `writing-tests-typescript` | Write TypeScript tests with typed mocks and behavior checks. |
| `writing-tests-react` | Use the React testing guides for component and story coverage. |
| `writing-tests-pg-data` | Write PG Data tests with database fixtures and integration coverage. |
| `backport-code` | Carry a fix across specifications, templates, and downstream repositories. |
| `security-deepsec` | Run a DeepSec security review and track remediation. |
| `new-project` | Scaffold and prepare a new repository from an Agent IX template. |
| `docs-figures` | Draw transparent light/dark SVG diagrams, charts and CSS animations for GitHub docs, clean or hand-sketched. |

Each skill states its own external prerequisites. Quoin is used for formal specification and evidence artifacts when installed; DeepSec is required for its named audit workflow. The `new-project` skill currently offers a public Rust scaffold through [rust-lib-cookiecutter](https://github.com/agent-ix/rust-lib-cookiecutter). Its other named Agent IX templates require separate authenticated access and are not part of the public support promise. `new-project` applies its own AGPL policy to generated projects; that does not change this plugin's MIT license.

These instructions reflect Agent IX conventions where stated. A target repository's own instructions and license remain authoritative for its code.

## Install

The repository is one plugin. The same `skills/` tree is used by all four hosts.

| Host | Install |
| --- | --- |
| Claude Code | `claude plugin marketplace add agent-ix/agent-plugins` then `claude plugin install dev-tools@agent-ix` |
| Codex | `codex plugin marketplace add agent-ix/agent-plugins` then `codex plugin add dev-tools@agent-ix` |
| GitHub Copilot CLI | `copilot plugin install agent-ix/dev-tools` |
| OpenCode | Add `"https://raw.githubusercontent.com/agent-ix/dev-tools/main/skills/"` to the `skills` array in your `opencode.jsonc`; the URL serves this repository's `skills/index.json` catalog. |

Claude uses `.claude-plugin/plugin.json`, Codex and Copilot use the root portable `plugin.json`, and OpenCode uses the remote skill catalog. The [Agent IX public marketplace](https://github.com/agent-ix/agent-plugins) pins reviewed versions for Claude and Codex. The `.codex-plugin/plugin.json` file supports older Codex plugin loaders. No host-specific copy of a skill is maintained.

## License

MIT. See [LICENSE](LICENSE). See [CONTENT_RIGHTS.md](CONTENT_RIGHTS.md) for source-content rules.
