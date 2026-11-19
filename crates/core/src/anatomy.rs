//! One run's time: waiting first, tools second, model last.
//! Recap retains its private interval helpers to avoid overlapping another change.

#[derive(Clone, Debug, PartialEq)]
pub struct Parts { pub waiting: Vec<(f64, f64)>, pub tools: Vec<(f64, f64)>, pub model: Vec<(f64, f64)> }

pub fn split(a: f64, b: f64, tools: &[(f64, f64)], asks: &[(f64, f64)]) -> Parts {
    let waiting = merged(asks, a, b);
    let tools = outside(tools, &waiting, a, b);
    let mut busy = waiting.clone();
    busy.extend_from_slice(&tools);
    let model = outside(&[(a, b)], &busy, a, b);
    Parts { waiting, tools, model }
}

pub fn merged(spans: &[(f64, f64)], lo: f64, hi: f64) -> Vec<(f64, f64)> {
    let mut clipped: Vec<(f64, f64)> = spans.iter().map(|&(a, b)| (a.max(lo), b.min(hi))).filter(|(a, b)| b > a).collect();
    clipped.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut pieces: Vec<(f64, f64)> = vec![];
    for (a, b) in clipped {
        match pieces.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => pieces.push((a, b)),
        }
    }
    pieces
}

/// Total length of a set of intervals, each clipped to `[lo, hi]`.
pub fn union(spans: &[(f64, f64)], lo: f64, hi: f64) -> f64 {
    merged(spans, lo, hi).iter().map(|(a, b)| b - a).sum()
}

/// The parts of `spans` within `[lo, hi]` that nothing in `cover` covers.
pub fn outside(spans: &[(f64, f64)], cover: &[(f64, f64)], lo: f64, hi: f64) -> Vec<(f64, f64)> {
    let cover = merged(cover, lo, hi);
    let mut left = vec![];
    for (mut a, b) in merged(spans, lo, hi) {
        for &(c, d) in &cover {
            if d <= a || c >= b {
                continue;
            }
            if c > a {
                left.push((a, c));
            }
            a = a.max(d);
        }
        if a < b {
            left.push((a, b));
        }
    }
    left
}

/// When a provider closes calls together at run end, their shared timestamp can
/// overstate how long each call took.
///
/// The rule, shared with the Tools view: among one thread's calls, group those with an end by
/// that end, exactly. In a group of two or more whose starts span more than 1,000 ms (latest
/// start minus earliest), every member was closed together. A group whose starts are within
/// 1,000 ms is a batch run in parallel and is left alone. `calls` is (start, end); the result
/// says, for each in order, whether it was closed together.
pub fn closed_together(calls: &[(f64, Option<f64>)]) -> Vec<bool> {
    let mut by_end: std::collections::HashMap<u64, (f64, f64, u32)> = std::collections::HashMap::new();
    for &(start, end) in calls {
        if let Some(end) = end {
            let group = by_end.entry(end.to_bits()).or_insert((f64::INFINITY, f64::NEG_INFINITY, 0));
            group.0 = group.0.min(start);
            group.1 = group.1.max(start);
            group.2 += 1;
        }
    }
    calls
        .iter()
        .map(|&(_, end)| end.and_then(|end| by_end.get(&end.to_bits())).is_some_and(|&(first, last, n)| n >= 2 && last - first > 1000.))
        .collect()
}

/// What kind of ask, for `waiting`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AskKind {
    /// An approval: it holds the run until answered.
    Approval,
    /// A question. `by_message` when T3 records it as answerable by a later message
    /// (`responseCapability.type = "message"`): the run may work past it. A missing,
    /// malformed or unfamiliar capability is not `by_message`.
    Question { by_message: bool },
    /// A secret card: it asks only while its own run is open.
    Secret,
    /// A link card: it stays open after its run, until the person writes in the thread again.
    Link,
}

/// One ask, as `waiting` needs it. Times are epoch milliseconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ask {
    pub kind: AskKind,
    pub start: f64,
    /// When it was resolved, cancelled, expired or failed; None while it is pending.
    pub end: Option<f64>,
    /// A secret card's own run: when it opened, and when it ended (None while open). A secret
    /// with no known run asks nothing.
    pub own_run: Option<(f64, Option<f64>)>,
    /// A link card: the first update of a genuine human message in its thread strictly after
    /// the card's start (role user, `createdBy = "user"`, no idle compaction).
    pub human_after: Option<f64>,
}

/// The stretches in which a thread was waiting on its person: the union of its blocking
/// intervals, each instant counted once however many asks cover it. One rule for every view
/// that counts waiting (Trace, Recap), agreed on 2026-11-18 with the thread that counts open
/// asks live, from T3's own rules:
///
/// - an approval, or a question not answerable by message: from asked to its end;
/// - a question answerable by message: never. T3 does not hold the run for it and settling
///   the thread cancels it, so charging the person for it would claim what nobody recorded.
///   (A question T3 turned into one at a restart was blocking before; its final record does
///   not say when it changed, so that part is not counted either.)
/// - a secret card: while it is pending and its own run is open (both ends of the run clip it);
///   with no known run, never;
/// - a link card: open from asked to its end or `human_after`, whichever is first, but
///   waiting only where no run of the thread is working (`working`): an idle gap with the
///   link pending counts, and a later run suspends it.
///
/// `working` is every interval in which a run of the thread was open, whichever task it
/// belongs to; `now` ends what is still open.
pub fn waiting(asks: &[Ask], working: &[(f64, f64)], now: f64) -> Vec<(f64, f64)> {
    let all: Vec<(f64, f64)> = asks.iter().flat_map(|ask| blocking(ask, working, now)).collect();
    merged(&all, f64::NEG_INFINITY, f64::INFINITY)
}

/// One ask's blocking intervals, by the rule of `waiting`.
pub fn blocking(ask: &Ask, working: &[(f64, f64)], now: f64) -> Vec<(f64, f64)> {
    let end = ask.end.unwrap_or(now);
    let pieces = match ask.kind {
        AskKind::Approval | AskKind::Question { by_message: false } => vec![(ask.start, end)],
        AskKind::Question { by_message: true } => vec![],
        AskKind::Secret => match ask.own_run {
            Some((run_start, run_end)) => vec![(ask.start.max(run_start), end.min(run_end.unwrap_or(now)))],
            None => vec![],
        },
        AskKind::Link => {
            let stop = ask.human_after.map_or(end, |human| end.min(human));
            outside(&[(ask.start, stop)], working, ask.start, stop)
        }
    };
    pieces.into_iter().filter(|(a, b)| b > a).collect()
}

/// Whether an ask holds the person at `now`: pending (no end, and for a link no human update
/// since), and blocking by the rule of `waiting` at that instant. This, not a stretch that
/// reaches `now`, decides that a thread is waiting: a stretch cannot tell an ask answered at
/// `now` from one still pending.
///
/// `working_now` is whether a run of the thread is open at this instant, from the runs' own
/// state, not from `working`: a run still open has its interval capped at `now` exactly as a
/// run that finished at `now` does, so the intervals cannot tell the two apart. The
/// intervals stay half-open for durations.
pub fn holds_now(ask: &Ask, working_now: bool, now: f64) -> bool {
    if ask.end.is_some() || ask.start > now {
        return false;
    }
    match ask.kind {
        AskKind::Approval | AskKind::Question { by_message: false } => true,
        AskKind::Question { by_message: true } => false,
        AskKind::Secret => ask.own_run.is_some_and(|(run_start, run_end)| run_start <= now && run_end.is_none()),
        AskKind::Link => ask.human_after.is_none() && !working_now,
    }
}

/// Whether an ask is still open at all: pending, and for a secret its run still open, for a
/// link no human update since. A question answerable by message is open but holds nobody,
/// and does not keep a task open.
pub fn still_open(ask: &Ask) -> bool {
    ask.end.is_none()
        && match ask.kind {
            AskKind::Approval | AskKind::Question { by_message: false } => true,
            AskKind::Question { by_message: true } => false,
            AskKind::Secret => ask.own_run.is_some_and(|(_, run_end)| run_end.is_none()),
            AskKind::Link => ask.human_after.is_none(),
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(kind: AskKind, start: f64, end: Option<f64>) -> Ask {
        Ask { kind, start, end, own_run: None, human_after: None }
    }

    #[test]
    fn a_link_waits_only_while_no_run_works() {
        // The example agreed with the open-asks thread: a link open at 2, runs [0,5) and
        // [8,11), a human update at 14. Open on [2,14); waiting on [5,8) and [11,14).
        let link = Ask { human_after: Some(14.), ..ask(AskKind::Link, 2., None) };
        let runs = [(0., 5.), (8., 11.)];
        assert_eq!(waiting(&[link], &runs, 100.), [(5., 8.), (11., 14.)]);
        // A concurrent blocking approval is counted once with it.
        let approval = ask(AskKind::Approval, 6., Some(12.));
        assert_eq!(waiting(&[link, approval], &runs, 100.), [(5., 14.)]);
        // Resolved before the human wrote: it ends there.
        let resolved = Ask { end: Some(9.), ..link };
        assert_eq!(waiting(&[resolved], &runs, 100.), [(5., 8.)]);
        // Still pending with no human update: open to now, waiting outside the runs.
        let pending = ask(AskKind::Link, 2., None);
        assert_eq!(waiting(&[pending], &runs, 20.), [(5., 8.), (11., 20.)]);
    }

    #[test]
    fn a_question_answerable_by_message_is_never_waiting() {
        let by_message = ask(AskKind::Question { by_message: true }, 0., None);
        // Not inside a run, not after the last one, not while open.
        assert!(waiting(&[by_message], &[(0., 5.)], 1_000.).is_empty());
        // A question that holds the run, and one whose capability was not recorded, are.
        let holding = ask(AskKind::Question { by_message: false }, 1., Some(4.));
        assert_eq!(waiting(&[by_message, holding], &[(0., 5.)], 1_000.), [(1., 4.)]);
        assert_eq!(waiting(&[ask(AskKind::Approval, 6., None)], &[], 10.), [(6., 10.)]);
    }

    #[test]
    fn live_review_secret_without_own_run() {
        let orphan = Ask { kind: AskKind::Secret, start: 2., end: Some(9.), own_run: None, human_after: None };
        assert!(waiting(&[orphan], &[], 10.).is_empty());
    }

    #[test]
    fn live_review_secret_cannot_precede_run() {
        let secret = Ask { kind: AskKind::Secret, start: 2., end: Some(9.), own_run: Some((5., Some(8.))), human_after: None };
        assert_eq!(waiting(&[secret], &[], 12.), [(5., 8.)]);
    }

    #[test]
    fn holding_now_is_decided_by_pending_asks_not_by_stretches_reaching_now() {
        let link = ask(AskKind::Link, 2., None);
        // Pending, no run working: holds. A run working: does not. Answered at now: does not.
        assert!(holds_now(&link, false, 10.));
        assert!(!holds_now(&link, true, 10.));
        assert!(!holds_now(&Ask { human_after: Some(10.), ..link }, false, 10.));
        let secret = Ask { own_run: Some((0., None)), ..ask(AskKind::Secret, 2., None) };
        assert!(holds_now(&secret, true, 10.));
        assert!(!holds_now(&Ask { own_run: Some((0., Some(5.))), ..secret }, false, 10.));
        assert!(!holds_now(&ask(AskKind::Question { by_message: true }, 2., None), false, 10.));
        assert!(holds_now(&ask(AskKind::Approval, 2., None), true, 10.));
        assert!(!still_open(&ask(AskKind::Question { by_message: true }, 2., None)) && still_open(&ask(AskKind::Approval, 2., None)));
    }

    #[test]
    fn a_secret_card_waits_only_while_its_run_is_open() {
        let secret = Ask { own_run: Some((0., Some(5.))), ..ask(AskKind::Secret, 2., None) };
        assert_eq!(waiting(&[secret], &[(0., 5.)], 100.), [(2., 5.)]);
        let cancelled = Ask { end: Some(3.), ..secret };
        assert_eq!(waiting(&[cancelled], &[(0., 5.)], 100.), [(2., 3.)]);
        let running = Ask { own_run: Some((0., None)), ..ask(AskKind::Secret, 2., None) };
        assert_eq!(waiting(&[running], &[(0., 50.)], 50.), [(2., 50.)]);
    }

    #[test]
    fn calls_ending_at_one_instant_from_different_starts_were_closed_together() {
        let end = 1_000_000.;
        let calls = [
            (0., Some(end)),          // closed together: its group starts span 600 s
            (600_000., Some(end)),    // closed together
            (900_000., Some(950_000.)), // its own end
            (700_000., None),         // still open
            (10., Some(2_000_000.)),  // a parallel batch: starts 990 ms apart, left alone
            (1_000., Some(2_000_000.)),
        ];
        assert_eq!(closed_together(&calls), [true, true, false, false, false, false]);
        // Just over a second apart is closed together.
        assert_eq!(closed_together(&[(0., Some(5000.)), (1001., Some(5000.))]), [true, true]);
        assert_eq!(closed_together(&[(0., Some(5000.))]), [false]);
    }
}
