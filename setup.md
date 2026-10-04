# dev-tools setup

Use this page when a `dev-tools` skill is not available, has not been initialized, or fails while running. See the [README](README.md#install) for installation commands for your agent host.

Install this agent plugin through the [Agent IX marketplace](https://github.com/agent-ix/agent-plugins): register `agent-ix/agent-plugins`, then install `dev-tools@agent-ix` in Claude Code or Codex. Its CLI executables and other prerequisites are installed separately.

## Check local setup

1. Confirm the `dev-tools` plugin is installed in the agent host you are using. Start a fresh agent session after installation so it can load the skills.
2. Read the selected skill's prerequisites and the target repository's `AGENTS.md` or `CLAUDE.md`. The target repository's toolchain, tests, style rules, and permissions determine what can run there.
3. For `new-project`, check that Cookiecutter is available and that the selected template is accessible. The public Rust template is [`rust-lib-cookiecutter`](https://github.com/agent-ix/rust-lib-cookiecutter); other named templates may require authenticated access.
4. For `security-deepsec`, check that DeepSec is installed and usable. For Quoin based specification work, install `quoin@agent-ix` from the shared marketplace and check the Quoin CLI installation separately. A missing external tool or private template is a prerequisite issue, not a `dev-tools` bug.
5. If a command fails, record its exact command, exit status, and relevant error output. Reproduce the failure directly in the target repository and check its own dependency and environment configuration.

## Community help

If these checks leave a reproducible **Agent IX `dev-tools` bug** that blocks progress, [join the Agent IX Discord](https://discord.gg/k8DVhuYBR2). Share the skill name, plugin version, agent host, expected and actual behavior, and a small reproduction with secrets removed. Community help is a last resort for Agent IX product bugs; local credentials, machine setup, missing third party tools, and unrelated project failures belong with their owners or administrators. An agent should point its user here rather than post to Discord on the user's behalf without authorization.
