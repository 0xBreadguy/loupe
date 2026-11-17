// Themes the tests judge the palette by: grayscale plus one ember, a colorful one, a light one.
import { parseColorsToml } from "../src/theme.ts"

export const THEMES: Record<string, Record<string, string>> = {
  // Grayscale plus one ember: the theme to judge by.
  solitude: parseColorsToml(`mode = "dark"
accent = "#798186"
selection = "#343d41"
background = "#101315"
foreground = "#cacccc"
red = "#565d60"
yellow = "#d9dbdc"
bright_red = "#de6145"
bright_yellow = "#c9c2b4"`),
  tokyo: { background: "#1a1b26", foreground: "#c0caf5", accent: "#7aa2f7", selection: "#33467c", red: "#f7768e",
    yellow: "#e0af68", green: "#9ece6a", blue: "#7aa2f7", magenta: "#bb9af7" },
  light: { mode: "light", background: "#fafafa", foreground: "#222222", accent: "#3366cc", red: "#cc3333",
    yellow: "#b58900", green: "#2e7d32", blue: "#1565c0", magenta: "#8e24aa" },
}
