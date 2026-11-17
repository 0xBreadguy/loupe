//! OpenTUI v0.8.3 terminal.zig color capabilities and renderer.zig palette search.
use std::collections::HashMap;
use t3_agents_core::cluster::Xtversion;

pub struct Colors {
    ansi256: bool,
    rgb: bool,
    screen: bool,
    palette: [u32; 256],
    cache: HashMap<u32, u8>,
}
impl Default for Colors {
    fn default() -> Self {
        Self::from_env(|_| None)
    }
}
impl Colors {
    pub fn from_process() -> Self {
        Self::from_env(|k| std::env::var(k).ok())
    }
    pub(crate) fn from_env(get: impl Fn(&str) -> Option<String>) -> Self {
        let term = get("TERM").unwrap_or_default();
        let remote = ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY", "MOSH_CONNECTION"]
            .iter()
            .any(|k| get(k).is_some());
        let rgb = cfg!(windows)
            || matches!(get("COLORTERM").as_deref(), Some("truecolor" | "24bit"))
            || (!remote && get("WT_SESSION").is_some());
        Self {
            ansi256: rgb || term.to_ascii_lowercase().contains("256color"),
            rgb,
            screen: get("STY").is_some() || (get("TMUX").is_none() && term.starts_with("screen")),
            palette: std::array::from_fn(fallback),
            cache: HashMap::new(),
        }
    }
    pub fn indexed(&self) -> bool {
        self.ansi256 && !self.rgb
    }
    pub fn version(&mut self, text: &str) -> bool {
        let Some(v) = Xtversion::parse(text) else {
            return false;
        };
        if v.name.eq_ignore_ascii_case("tmux") {
            self.screen = false;
        }
        if [
            "kitty", "ghostty", "WezTerm", "foot", "XTerm", "iTerm2", "tmux",
        ]
        .iter()
        .any(|s| v.name.eq_ignore_ascii_case(s))
        {
            return self.truecolor();
        }
        false
    }
    pub fn truecolor(&mut self) -> bool {
        let before = self.indexed();
        if !self.screen {
            self.rgb = true;
            self.ansi256 = true;
        }
        before != self.indexed()
    }
    pub fn palette(&mut self, index: u8, rgb: u32) -> bool {
        if index >= 16 || self.palette[index as usize] == rgb {
            return false;
        }
        self.palette[index as usize] = rgb;
        self.cache.clear();
        self.indexed()
    }
    pub fn nearest(&mut self, rgb: u32) -> u8 {
        *self.cache.entry(rgb).or_insert_with(|| {
            self.palette
                .iter()
                .enumerate()
                .min_by_key(|(_, c)| {
                    [0, 8, 16]
                        .iter()
                        .map(|s| {
                            let d = ((rgb >> s) & 255) as i32 - ((**c >> s) & 255) as i32;
                            d * d
                        })
                        .sum::<i32>()
                })
                .unwrap()
                .0 as u8
        })
    }
}
pub fn fallback(i: usize) -> u32 {
    const BASE: [u32; 16] = [
        0, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080, 0xc0c0c0, 0x808080,
        0xff0000, 0x00ff00, 0xffff00, 0x0000ff, 0xff00ff, 0x00ffff, 0xffffff,
    ];
    const LEVEL: [u32; 6] = [0, 95, 135, 175, 215, 255];
    if i < 16 {
        BASE[i]
    } else if i < 232 {
        let n = i - 16;
        (LEVEL[n / 36] << 16) | (LEVEL[n / 6 % 6] << 8) | LEVEL[n % 6]
    } else {
        (8 + (i as u32 - 232) * 10) * 0x010101
    }
}

/// terminal.zig:1490: only successful RGB/Tc with absent or nonempty even hex value.
pub fn xtgettcap(body: &[u8]) -> bool {
    let Some(body) = body.strip_prefix(b"1+r") else {
        return false;
    };
    let mut parts = body.splitn(2, |b| *b == b'=');
    let name = parts.next().unwrap();
    if !name.eq_ignore_ascii_case(b"524742") && !name.eq_ignore_ascii_case(b"5463") {
        return false;
    }
    parts
        .next()
        .is_none_or(|v| !v.is_empty() && v.len() % 2 == 0 && v.iter().all(u8::is_ascii_hexdigit))
}
pub fn osc_palette(body: &[u8]) -> Option<(u8, u32)> {
    let text = std::str::from_utf8(body).ok()?.strip_prefix("4;")?;
    let (index, value) = text.split_once(';')?;
    let index = index.parse::<u8>().ok()?;
    let rgb = if let Some(hex) = value.strip_prefix('#') {
        if hex.len() != 6 {
            return None;
        }
        u32::from_str_radix(hex, 16).ok()?
    } else {
        let mut rgb = 0;
        let parts: Vec<_> = value.strip_prefix("rgb:")?.split('/').collect();
        if parts.len() != 3 {
            return None;
        }
        for c in parts {
            if c.is_empty() || c.len() > 4 {
                return None;
            }
            let n = u32::from_str_radix(c, 16).ok()?;
            let max = (1u32 << (c.len() * 4)) - 1;
            rgb = (rgb << 8) | ((n * 255 + max / 2) / max);
        }
        rgb
    };
    Some((index, rgb))
}

/// Put color queries before the final reply sentinel. Screen prints DCS bodies, so never ask it.
pub fn startup_probe() -> Vec<u8> {
    let colors = Colors::from_process();
    let mut out = crate::caps::PROBE[..crate::caps::PROBE.len() - 3].to_vec();
    if !cfg!(windows) && !colors.rgb && !colors.screen {
        out.extend_from_slice(b"\x1bP+q524742\x1b\\\x1bP+q5463\x1b\\");
    }
    if colors.indexed() {
        out.extend_from_slice(b"\x1b]4;0;?\x07");
    }
    out.extend_from_slice(b"\x1b[c");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capability_rules_and_palette_ties() {
        let mut c = Colors::from_env(|k| (k == "TERM").then(|| "XTERM-256color".into()));
        assert!(c.indexed());
        assert_eq!(c.nearest(0), 0); // also slot 16: lowest index wins
        assert_eq!(c.nearest(0xff0000), 9); // also slot 196
        assert!(c.palette(0, 0x123456));
        assert_eq!(c.nearest(0x123456), 0);
        assert_eq!(c.nearest(0), 16); // old cache was invalidated
        assert!(!c.version("Alacritty 0.17.0"));
        assert!(c.version("kitty(0.49.2)"));
        assert!(!c.indexed());
        let mut c = Colors::from_env(|k| (k == "TERM").then(|| "screen-256color".into()));
        assert!(!c.truecolor());
        assert!(!c.version("foot(1.28.0)"));
        assert!(c.indexed());
        assert!(c.version("tmux 3.8"));
        assert!(!Colors::default().indexed());
    }
    #[test]
    fn replies_require_valid_values() {
        for s in ["1+r524742", "1+r5463=31", "1+r524742=0010"] {
            assert!(xtgettcap(s.as_bytes()));
        }
        for s in [
            "0+r524742",
            "1+r524742=",
            "1+r5463=1",
            "1+r5463=xx",
            "1+r524742;5463",
        ] {
            assert!(!xtgettcap(s.as_bytes()));
        }
        assert_eq!(osc_palette(b"4;2;rgb:1/8000/ffff"), Some((2, 0x1180ff)));
        assert_eq!(osc_palette(b"4;15;#abcdef"), Some((15, 0xabcdef)));
        assert_eq!(osc_palette(b"4;256;#abcdef"), None);
    }
}

#[cfg(test)]
mod integration_tests {
    use crate::{Event, Parser, Report};
    use std::time::{Duration, Instant};
    #[test]
    fn color_replies_are_reports_even_late_and_cut() {
        let now = Instant::now();
        for (bytes, report) in [
            (&b"\x1bP1+r524742\x1b\\"[..], Report::Truecolor),
            (
                &b"\x1b]4;3;rgb:ffff/8000/0000\x07"[..],
                Report::Palette {
                    index: 3,
                    rgb: 0xff8000,
                },
            ),
        ] {
            for cut in 1..bytes.len() {
                let mut p = Parser::new();
                p.expect_replies(now + Duration::from_millis(250));
                let late = now + Duration::from_secs(1);
                let mut got = p.push(&bytes[..cut], late);
                got.extend(p.push(&bytes[cut..], late + Duration::from_millis(1)));
                assert_eq!(got, vec![Event::Report(report.clone())], "cut {cut}");
            }
        }
    }
}
