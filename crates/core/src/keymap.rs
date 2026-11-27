//! The keys the product's screens share: Storage, Insights, Telemetry and Trace list them in
//! their help in one order and in the same words, the order and words of the key table in
//! `docs/usage.md`. The dashboard's own help is the original's (`oracle/`) and is not built here.
//!
//! A screen lists only the keys it answers, and may add its own (`←` `→`, Space) after the
//! shared key nearest in meaning.
use crate::terminal::{self, Canvas, Palette, Rgb};

pub const MOVE: &str = "↑↓ / j k";
pub const ENDS: &str = "g G / Home End";
pub const PAGE: &str = "J K / PgDn PgUp";
pub const VIEW: &str = "Tab / ⇧Tab";
pub const JUMP: &str = "s i e w";
pub const OPEN: &str = "⏎ / o";
pub const HAND: &str = "f";
pub const READ: &str = "r";
pub const LOOK: &str = "c C / m";
pub const HELP: &str = "? / h";
pub const BACK: &str = "Esc / q";

/// The shared keys in the table's order.
pub const ORDER: [&str; 11] = [MOVE, ENDS, PAGE, VIEW, JUMP, OPEN, HAND, READ, LOOK, HELP, BACK];

/// Where the descriptions start: one cell after the widest shared key at column 2.
pub const LABEL_X: i32 = 18;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Storage,
    Insights,
    Telemetry,
    Trace,
}

impl Screen {
    fn key(self) -> char {
        match self {
            Screen::Storage => 's',
            Screen::Insights => 'i',
            Screen::Telemetry => 'e',
            Screen::Trace => 'w',
        }
    }
}

/// `s i e w` on `screen`: the other three by name, and the screen's own key back.
fn jump(screen: Screen) -> String {
    let others: Vec<String> = [
        (Screen::Storage, "Storage"),
        (Screen::Insights, "Insights"),
        (Screen::Telemetry, "Telemetry"),
        (Screen::Trace, "Trace"),
    ]
    .into_iter()
    .filter(|(s, _)| *s != screen)
    .map(|(s, name)| format!("{} {name}", s.key()))
    .collect();
    format!("{} · {} dashboard", others.join(" · "), screen.key())
}

const LOOK_WORDS: &str = "theme next · back / calm mode";
const HELP_WORDS: &str = "close help";

/// The help lines of `screen`. `cleanup`: Storage can switch to T3's cleanup settings.
pub fn lines(screen: Screen, cleanup: bool) -> Vec<(&'static str, String)> {
    let s = |text: &str| text.to_owned();
    match screen {
        Screen::Storage => {
            let mut v = vec![
                (MOVE, s("select a row; a click or the wheel too")),
                (ENDS, s("first / last row")),
                (PAGE, s("a page of rows")),
            ];
            if cleanup {
                v.push((VIEW, s("T3 cleanup settings ↔ folders")));
            }
            v.extend([
                (JUMP, jump(screen)),
                (OPEN, s("open the selected thread in T3")),
                ("␣", s("fold or unfold a project (⏎ too, on its row)")),
                (HAND, s("hand the selection to a T3 agent (asks first)")),
                (READ, s("measure again; reread T3's cleanup settings")),
                (LOOK, s(LOOK_WORDS)),
                (HELP, s(HELP_WORDS)),
                (BACK, s("back: settings to folders, then the dashboard / quit")),
            ]);
            v
        }
        Screen::Insights => vec![
            (MOVE, s("scroll a row; the wheel too")),
            (ENDS, s("top / bottom")),
            (PAGE, s("a page")),
            (VIEW, s("Recap ↔ Tools; or click its name")),
            ("← →", s("previous / next period; or click it")),
            (JUMP, jump(screen)),
            (HAND, s("hand this report to a T3 agent (asks first)")),
            (READ, s("read again; the result stays till then")),
            (LOOK, s(LOOK_WORDS)),
            (HELP, s(HELP_WORDS)),
            (BACK, s("back to the dashboard / quit")),
        ],
        Screen::Telemetry => vec![
            (MOVE, s("select a stream · scroll Sources; click, wheel")),
            (ENDS, s("first / last")),
            (PAGE, s("a page")),
            (VIEW, s("Telemetry ↔ Sources; or click its name")),
            (JUMP, jump(screen)),
            ("⏎ / ␣", s("show or hide the suggested commands")),
            (HAND, s("fix instructions to a T3 agent (asks first)")),
            (READ, s("check again; it also does when a file changes")),
            (LOOK, s(LOOK_WORDS)),
            (HELP, s(HELP_WORDS)),
            (BACK, s("back to the dashboard / quit")),
        ],
        Screen::Trace => vec![
            (MOVE, s("select a step; a click or the wheel too")),
            (ENDS, s("first / last step")),
            (PAGE, s("scroll steps")),
            ("← →", s("previous / next task")),
            (JUMP, jump(screen)),
            (OPEN, s("open a delegate's trace, or the thread in T3")),
            (HAND, s("diagnose this task with a T3 agent (asks first)")),
            (READ, s("read the trace again")),
            ("p", s("pause the dashboard's updates")),
            (LOOK, s(LOOK_WORDS)),
            (HELP, s(HELP_WORDS)),
            (BACK, s("back one trace, then the dashboard / quit")),
        ],
    }
}

/// Draw `lines` from row `y`, keys at column 2 and words at [`LABEL_X`], stopping before row
/// `end`. Returns the row after the last line drawn.
pub fn draw(c: &mut Canvas, mut y: i32, end: i32, lines: &[(&str, String)], p: &Palette, words: Rgb) -> i32 {
    for (key, label) in lines {
        if y >= end {
            break;
        }
        c.text_bold(2, y, key, p.accent);
        c.text(LABEL_X, y, &terminal::fit(label, (c.w as i32 - LABEL_X - 1).max(0) as usize), words);
        y += 1;
    }
    y
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every screen lists the shared keys it answers in the table's order, each once.
    #[test]
    fn every_screen_lists_the_shared_keys_in_the_tables_order() {
        for screen in [Screen::Storage, Screen::Insights, Screen::Telemetry, Screen::Trace] {
            for cleanup in [false, true] {
                let shared: Vec<usize> = lines(screen, cleanup)
                    .iter()
                    .filter_map(|(key, _)| ORDER.iter().position(|k| k == key))
                    .collect();
                assert!(shared.windows(2).all(|w| w[0] < w[1]), "{screen:?}: {shared:?}");
                for key in [MOVE, ENDS, PAGE, JUMP, READ, LOOK, HELP, BACK] {
                    assert!(lines(screen, cleanup).iter().any(|(k, _)| *k == key), "{screen:?} lacks {key}");
                }
            }
        }
        assert!(!lines(Screen::Storage, false).iter().any(|(k, _)| *k == VIEW));
        assert!(lines(Screen::Storage, true).iter().any(|(k, _)| *k == VIEW));
    }

    #[test]
    fn the_jump_line_names_the_others_and_its_own_key_back() {
        assert_eq!(jump(Screen::Storage), "i Insights · e Telemetry · w Trace · s dashboard");
        assert_eq!(jump(Screen::Trace), "s Storage · i Insights · e Telemetry · w dashboard");
    }

    #[test]
    fn shared_keys_fit_before_the_words() {
        for key in ORDER {
            assert!(2 + terminal::width(key) < LABEL_X as usize, "{key}");
        }
    }
}
