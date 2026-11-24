# Built-in themes

Each file is a theme Loupe compiles in. The format is an Omarchy
`colors.toml` plus a few optional keys of the dashboard's own; it's described
in [docs/themes.md](../docs/themes.md), with how to make your own.

- `frontier.toml`, `paper.toml`, `phosphor.toml`: Loupe's own.
- `dracula.toml`: the Dracula palette (MIT).
- Every other file: one of Omarchy's default themes, its `colors.toml` copied
  unchanged from the commit named at the top (MIT), with provider colors added
  at the end so the dashboard can tell Claude, OpenAI, Grok, Gemini, "needs
  you" and the warning apart.

To add one, see [CONTRIBUTING.md](../CONTRIBUTING.md#themes).
