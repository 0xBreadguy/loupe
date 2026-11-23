# Themes

T3 Agents ships 27 themes and reads any Omarchy theme. On Omarchy it follows
your live theme, as it always has. Everywhere else it uses Frontier in the
dark and Paper in the light, and switches between them with your terminal.

## Pick one

- **`c` and `C`** in the dashboard step to the next and previous theme. The
  choice is saved in `~/.config/t3-agents/config.toml` (or
  `$XDG_CONFIG_HOME/t3-agents/`). With a dark and light pair, `c` changes the
  theme for the mode you're in.
- **`--theme NAME`** for one run, **`T3_AGENTS_THEME=NAME`** for a shell or a
  launcher, or **`theme = "NAME"`** in the config file.
- **`t3-agents --themes`** lists every theme and marks the one in use.

What `NAME` can be:

| Name | What it is |
| --- | --- |
| `tokyo-night`, `Tokyo Night` | a built-in theme, one of yours, or an installed Omarchy theme |
| `~/themes/mine.toml` | a theme file anywhere |
| `omarchy` | the live Omarchy theme (the plain gray one where there's no Omarchy) |
| `dark:frontier,light:paper` | one theme for dark mode and one for light |

The first of these that's set wins: `--theme`, `T3_AGENTS_THEME`, the config
file, the live Omarchy theme, then `dark:frontier,light:paper`. A name that
doesn't exist stops `--theme` with an error. In the variable or the config
file it's skipped, and the dashboard says so when it starts.

## Light and dark

With a pair, the dashboard picks the theme for the mode you're in:

- **On Omarchy**, the mode is the live theme's (`mode = "light"`, or a light
  background). Switch Omarchy to a light theme and the dashboard follows.
- **Elsewhere**, it asks the terminal for its background when it starts
  (`OSC 11`) and asks to be told when the terminal's color scheme changes
  (mode 2031), as OpenTUI does. A terminal that doesn't answer gets the dark
  theme.

## The built-in themes

| Theme | Mode | From |
| --- | --- | --- |
| `frontier` | dark | T3 Agents: near-black and graphite, color only where it means something |
| `paper` | light | T3 Agents: a printed report on warm paper |
| `phosphor` | dark | T3 Agents: an amber CRT, made for `x` |
| `dracula` | dark | the Dracula spec |
| `catppuccin`, `ethereal`, `everforest`, `gruvbox`, `hackerman`, `jade-horse`, `kanagawa`, `last-horizon`, `lumon`, `matte-black`, `miasma`, `nord`, `retro-82`, `ristretto`, `solitude`, `starship`, `tokyo-night`, `vantablack` | dark | Omarchy |
| `catppuccin-latte`, `flexoki-light`, `lupine`, `rose-pine`, `white` | light | Omarchy |

The files are in [`themes/`](../themes/). Omarchy's 23 are its own
`colors.toml` files, unchanged, with provider colors added at the end. A
terminal palette doesn't know which of its colors a dashboard needs kept apart.
Run through the dashboard as they ship, 11 of Omarchy's themes paint Grok in
the accent's hue, 11 lose their own colors for providers, and a few give two
roles the same color. The added colors come from each theme's own palette
where they can, and from colors close to it where they can't. Nine themes also
get a warning color, because the one the dashboard would pick sits too close
to their red.

When your live Omarchy theme is one of these and you haven't edited it, the
dashboard uses the same added colors.

Every built-in theme keeps Claude, OpenAI, Grok, Gemini, "needs you" and the
warning apart for colorblind readers (protanopia and deuteranopia), and for
everyone else. Catppuccin is the one near miss: its own peach sits a little
close to its yellow (ΔE 11.2 against a goal of 12).

Gemini is blue in every built-in theme, and at least 12 from Grok and from
OpenAI for everyone, 8 for colorblind readers. In Frontier it's Google's blue,
`#4285f4`. Seven themes have a blue accent (Catppuccin, Catppuccin Latte,
Flexoki Light, Lupine, Nord, Rose Pine and Tokyo Night), so Gemini shares the
context bars' hue there. It's picked to stay clear of every shade the bars
use, and `--theme-check` notes the shared hue.

## Make your own

1. Make `~/.config/t3-agents/themes/mine.toml`. Start from a file in
   [`themes/`](../themes/), or from three lines:

   ```toml
   base = "tokyo-night"
   claude = "#d97757"
   name = "Tokyo, my way"
   ```

2. Run `t3-agents --demo --theme mine`. Save the file and the dashboard
   redraws within a second.
3. Run `t3-agents --theme-check mine` to see contrast, colorblind separation,
   unknown keys, and any color the dashboard changed and why.

A theme is an Omarchy `colors.toml`, so any Omarchy theme's file works as is.
The dashboard reads these keys:

| Key | What it colors | Left out |
| --- | --- | --- |
| `background`, `foreground` | the page and the text; the dimmer text is worked out from these | gray |
| `accent` | context bars, big numbers, key hints | `blue` |
| `selection` | the selected row | a step from the background |
| `mode` | `"light"` or `"dark"` | guessed from the background |
| `hot` | "needs you": its tile, the `◆`, meters past 90% | `red` or `bright_red` |
| `warn` | a limit drawing near, a stalled agent, meters past 75% | `yellow` or `bright_yellow` |
| `claude`, `openai`, `grok`, `gemini`, `other` | each provider's dot and lane | `magenta`, `green`, `blue`, a blue of its own (below), a gray |
| `name` | what the theme is called in the list | the file's name |
| `base` | a theme to start from; every key in this file overrides it | none |

Values are `"#rrggbb"`. The dashboard keeps text readable: the text colors,
`accent`, `hot` and `warn` are nudged until they read at 4.5:1 on the
background, and `--theme-check` says when it did.

Gemini always gets a blue. Left out, it takes the first of these that stands
apart from Claude, OpenAI, Grok, "needs you" and the warning (12 for everyone,
6 for colorblind readers) and reads at 3:1: the theme's `blue`, its
`bright_blue`, then Google's blue `#4285f4`, `#2867e4`, `#83c4ff`, `#1e4eac`
and `#113585`. Grok usually holds the theme's `blue`, so Gemini usually moves on
to one of the others. If none clears both, it takes the stock blue that comes
closest.

### In an Omarchy theme

Put the dashboard's keys in a `t3-agents.toml` beside the theme's
`colors.toml`. Omarchy copies it into the live theme like any other file the
theme ships, and the dashboard reads it from there. It wins over the colors
added for Omarchy's own themes.

To give every Omarchy theme the same rule, make an Omarchy template,
`~/.config/omarchy/themed/t3-agents.toml.tpl`, and Omarchy writes it into each
theme you switch to:

```toml
claude = "{{ orange }}"
openai = "{{ cyan }}"
grok = "{{ magenta }}"
gemini = "{{ blue }}"
```

## Checking a theme

```text
$ t3-agents --theme-check frontier
Frontier (built in) · dark
…
Separation of Claude, OpenAI, Grok, Gemini, needs you and warning (OKLab ΔE × 100)
  colorblind      10.8  Grok vs Gemini: comfortable (6 floor, 8 comfortable)
  normal vision   14.0  Grok vs Gemini: fine (12 floor)

Notes
  - hot #bf3ca4 is drawn #c550ad so the needs you text reads at 4.5:1
```

Separation is the distance between two colors in OKLab, times 100. Under 6, a
colorblind reader may see one color; 8 and up is easy. It's measured for
protanopia and deuteranopia, simulated after Machado, Oliveira and Fernandes
(2009). `--themes` calls a theme colorblind-safe when every pair is at least 6
apart.

## The original's colors

`T3_AGENTS_NO_THEMES=1` puts back the Node.js version's behavior: the live
Omarchy theme or plain gray, no `c`, and no theme options. The checks that
compare the two programs run with it.
