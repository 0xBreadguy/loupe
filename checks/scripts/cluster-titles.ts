// Thread titles made of grapheme clusters, for terminal-caps-parity.ts and terminal-parity.ts.
//
// Every kind of grapheme cluster the layout handles (crates/core/src/cluster.rs), beside wide text,
// at the end of a title cut to the list's width (the prefixes move the cut across them), and right
// after a wide character. A title reaches the screen through clean() (joiners and tags become
// spaces) and Canvas.text (variation selectors and U+0300..U+036F take no cell), which is what
// the dashboard can draw. The original regroups each run of styled cells into clusters and packs
// it to the left; the Rust presenter must put the same text in the same cells with the same widths.
export const CLUSTERS = [
  "tone \u{1f44d}\u{1f3fd} \u{1f44b}\u{1f3ff} \u270b\u{1f3fb} \u261d\u{1f3fc} x\u{1f3fe} \u{1f44d}\u{1f3fd}\u{1f3fd} end",
  "zwj \u{1f468}\u200d\u{1f469}\u200d\u{1f467}\u200d\u{1f466} \u{1f3f3}\ufe0f\u200d\u{1f308} \u{1f9d1}\u200d\u{1f4bb} \u{1f469}\u{1f3fd}\u200d\u{1f52c} \u{1f3f4}\u200d\u2620\ufe0f end",
  "flags \u{1f1ef}\u{1f1f5}\u{1f1e7}\u{1f1f7} \u{1f1fa}\u{1f1f8}\u{1f1ec} \u{1f1eb} end",
  "keys 1\ufe0f\u20e3 #\ufe0f\u20e3 *\u20e3 9\u20e3 end",
  "vs \u2764\ufe0f \u2764\ufe0e \u263a\ufe0f \u00a9\ufe0f \u{1f600}\ufe0e \u231a\ufe0f a\ufe0e \u2615 end",
  "marks e\u0301\u0303 \u0489a \u1ab5b x\u20d7 a\ua670 o\u20dd end",
  "tags \u{1f3f4}\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f} \u{1f3f4}\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f} end",
  "jamo \uac01 \uac00\u11a8 \u1100\u1161\u11a8 \u115f\u1160 \ua960\ud7b0 \u1161 end",
  "indic \u0915\u094d\u0937 \u0915\u094d\u200d\u0937\u093f \u0928\u093e \u0e01\u0e33 \u0995\u09cd\u09b7 end",
  "after wide \u65e5\u{1f44d}\u{1f3fd}\u672c\u{1f1ef}\u{1f1f5}\u8a9e1\u20e3\u4f60\u093e\u{1f321} end",
  ...[30, 33, 36, 39, 42, 45, 48, 51, 54].map((n, i) => "cut ".padEnd(n - 6 + (i % 3), "abcdefghij") + ["\u{1f44d}\u{1f3fd}\u{1f1ef}\u{1f1f5}1\u20e3\u65e5", "\u65e5\u{1f1ef}\u{1f1f5}\u{1f44b}\u{1f3ff}\u0915\u094d\u0937", "\u{1f3f3}\ufe0f\u{1f308}\uac00\u11a8\u261d\u{1f3fc}x"][i % 3]! + " tail words after the cut"),
]
