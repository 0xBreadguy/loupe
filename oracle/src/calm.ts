// Calm mode starts on by itself when Omarchy's animations are turned off.
import { existsSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"

/**
 * Whether Omarchy's animations are off (`omarchy toggle animations`). The toggle
 * leaves a flag file; its script writes under ~/.local/state while Hyprland reads
 * XDG_STATE_HOME, so either place counts. Animations turned off in a user's own
 * Hyprland config are not detected; --calm and the m key cover those.
 */
export function animationsOff(env: NodeJS.ProcessEnv = process.env): boolean {
  const flag = join("omarchy", "toggles", "hypr", "no-animations.lua")
  const states = [env.XDG_STATE_HOME, join(env.HOME || homedir(), ".local", "state")].filter((d): d is string => !!d)
  return states.some((dir) => existsSync(join(dir, flag)))
}
