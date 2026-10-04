# dev-tools

[![Agent IX Plugins](https://github.com/agent-ix/agent-plugins/raw/refs/heads/main/assets/agent-ix-plugins.svg)](https://github.com/agent-ix/agent-plugins)

Focused review, code guidance, security, backporting, and scaffolding skills.

## Skills

- `code-review`, `rust-review`, `review-react`, `rust-style`
- `writing-tests-python`, `writing-tests-typescript`, `writing-tests-react`, `writing-tests-pg-data`
- `backport-code`, `security-deepsec`, `new-project`

Each skill states its own external prerequisites. Quoin is used for formal specification and evidence artifacts when installed; DeepSec is required for its named audit workflow. The `new-project` skill currently offers a public Rust scaffold through [rust-lib-cookiecutter](https://github.com/agent-ix/rust-lib-cookiecutter). Its other named Agent IX templates require separate authenticated access and are not part of the public support promise. `new-project` applies its own AGPL policy to generated projects; that does not change this plugin's MIT license.

These instructions reflect Agent IX conventions where stated. A target repository's own instructions and license remain authoritative for its code.

## Install

The repository is one plugin. The same `skills/` tree is used by all four hosts.

| Host | Install |
| --- | --- |
| Claude Code | `claude plugin marketplace add agent-ix/agent-plugins` then `claude plugin install dev-tools@agent-ix-public` |
| Codex | `codex plugin marketplace add agent-ix/agent-plugins` then `codex plugin add dev-tools@agent-ix-public` |
| GitHub Copilot CLI | `copilot plugin install agent-ix/dev-tools` |
| OpenCode | Add `"https://raw.githubusercontent.com/agent-ix/dev-tools/main/skills/"` to the `skills` array in your `opencode.jsonc`; the URL serves this repository's `skills/index.json` catalog. |

Claude uses `.claude-plugin/plugin.json`, Codex and Copilot use the root portable `plugin.json`, and OpenCode uses the remote skill catalog. The [Agent IX public marketplace](https://github.com/agent-ix/agent-plugins) pins reviewed versions for Claude and Codex. The `.codex-plugin/plugin.json` file supports older Codex plugin loaders. No host-specific copy of a skill is maintained.

## License

MIT. See [LICENSE](LICENSE). See [CONTENT_RIGHTS.md](CONTENT_RIGHTS.md) for source-content rules.
