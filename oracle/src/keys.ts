// Numpad keys arrive under their own names when the terminal reports them
// distinctly (the kitty keyboard protocol, which foot and others speak): Enter is
// "kpenter", and with Num Lock off the arrows are "kpup" and so on. The
// dashboard treats each like its main-keyboard twin.
const KEYPAD: Record<string, string> = {
  kpenter: "return",
  kpup: "up",
  kpdown: "down",
  kpleft: "left",
  kpright: "right",
  kphome: "home",
  kpend: "end",
  kppageup: "pageup",
  kppagedown: "pagedown",
}

export const keyName = (name: string | undefined): string => (name ? KEYPAD[name] ?? name : "")
