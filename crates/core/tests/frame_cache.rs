use t3_agents_core::{
    model::{Changed, Event, Item, Snapshot, State, RECENT_MS},
    ui,
};
fn compare(cached: &State, reference: &State, w: usize, h: usize, step: usize) {
    let (a, sa) = ui::draw_grid(cached, w, h, &cached.palette);
    let (b, sb) = ui::draw_grid(reference, w, h, &reference.palette);
    assert_eq!(a.cells, b.cells, "canvas step {step}, {w}x{h}");
    assert_eq!(sa, sb, "surface step {step}, {w}x{h}");
    assert_eq!(
        cached.first_seen, reference.first_seen,
        "first_seen step {step}"
    );
    assert_eq!(cached.shimmer, reference.shimmer, "shimmer step {step}");
    assert_eq!(cached.acked, reference.acked, "acked step {step}");
    assert_eq!(cached.waits, reference.waits, "waits step {step}");
    assert_eq!(cached.tween, reference.tween, "tween step {step}");
    *cached.surface.borrow_mut() = sa;
    *reference.surface.borrow_mut() = sb;
}
#[test]
fn differential_sequences() {
    let demo: Snapshot =
        serde_json::from_str(include_str!("../../../checks/fixtures/demo.json")).unwrap();
    let edge: Snapshot = serde_json::from_str(include_str!("fixtures/edge.json")).unwrap();
    let mut large = demo.clone();
    large.agents = (0..3072)
        .map(|i| {
            let mut a = (*demo.agents[i % demo.agents.len()]).clone();
            a.id = format!("synthetic-{i}");
            a.parent = if i % 16 == 0 {
                None
            } else {
                Some(format!("synthetic-{}", i - i % 16))
            };
            std::sync::Arc::new(a)
        })
        .collect();
    let mut random = 0xd39a47a39u64;
    for snap in [demo, edge, large] {
        let mut a = State::new(snap);
        a.follow = false;
        let mut b = a.clone();
        b.cache_enabled = false;
        // Exact equality and both adjacent representable instants, in both directions.
        let epoch = a.now;
        let mut thresholds = vec![
            epoch,
            epoch + 100.,
            epoch + 120_000.,
            epoch + 300_000.,
            epoch + 480_000.,
            epoch + RECENT_MS,
        ];
        for agent in a.snap.agents.iter().take(40) {
            thresholds.push(t3_agents_core::model::last_seen(agent) + RECENT_MS);
            if let Some(t) = agent
                .shelf
                .as_ref()
                .and_then(|s| t3_agents_core::model::ms(s.snoozed_until.as_deref()))
            {
                thresholds.push(t);
            }
            let last = agent
                .events
                .iter()
                .map(|e| e.t)
                .fold(0f64, f64::max)
                .max(t3_agents_core::model::ms(agent.started.as_deref()).unwrap_or(0.))
                .max(
                    agent
                        .now
                        .as_ref()
                        .and_then(|i| t3_agents_core::model::ms(Some(&i.updated_at)))
                        .unwrap_or(0.),
                )
                .max(
                    agent
                        .items
                        .first()
                        .and_then(|i| t3_agents_core::model::ms(Some(&i.updated_at)))
                        .unwrap_or(0.),
                );
            thresholds.extend([last + 120_000., last + 480_000.]);
            if let Some(t) = agent.now.as_ref().and_then(|i| {
                t3_agents_core::model::ms(i.started_at.as_deref())
                    .or_else(|| t3_agents_core::model::ms(Some(&i.updated_at)))
            }) {
                thresholds.push(t + 300_000.);
            }
            for event in agent.events.iter().take(3) {
                thresholds.push(event.t - 5000.);
            }
        }
        for at in thresholds
            .into_iter()
            .flat_map(|t| [t.next_down(), t, t.next_up(), t])
        {
            a.now = at;
            b.now = at;
            compare(&a, &b, 233, 55, usize::MAX);
        }
        a.now = epoch;
        b.now = epoch;
        for i in 0..720 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            let now = a.now;
            match random % 15 {
                0 | 1 | 2 => {
                    let keys = [
                        "down", "up", "end", "home", "space", "left", "right", "a", "t", "?",
                        "escape", "p", "m", "x", "pagedown",
                    ];
                    let key = keys[(random >> 8) as usize % keys.len()];
                    a.action(key);
                    b.action(key);
                }
                3 => {
                    let id = a.snap.agents[(random >> 8) as usize % a.snap.agents.len()]
                        .id
                        .clone();
                    a.pick_at(&id, now);
                    b.pick_at(&id, now);
                }
                4 | 5 | 6 => {
                    let steps = [
                        0., 1., 100., 9999., 10000., 120000., 480000., RECENT_MS, -1000.,
                    ];
                    let at = now + steps[(random >> 8) as usize % steps.len()];
                    a.advance_frame(at, false);
                    b.advance_frame(at, false);
                }
                7 | 8 => {
                    let mut snap = a.snap.clone();
                    let n = (random >> 8) as usize % snap.agents.len();
                    snap.agent_mut(n).status =
                        if i % 2 == 0 { "running" } else { "completed" }.into();
                    snap.agent_mut(n).events.push(Event {
                        t: now + 5000.,
                        cat: "shell".into(),
                        failed: i % 3 == 0,
                    });
                    snap.at = now + 100.;
                    let hint = if i % 3 == 0 {
                        Changed::Everything
                    } else if i % 3 == 1 {
                        Changed::Agents(vec!["deliberately-wrong-id".into()])
                    } else {
                        Changed::Agents(snap.agents.iter().map(|a| a.id.clone()).collect())
                    };
                    a.apply(snap.clone(), &hint);
                    b.replace(snap);
                }
                _ => {
                    let mut snap = a.snap.clone();
                    let n = (random >> 8) as usize % snap.agents.len();
                    match random % 15 {
                        9 => {
                            let mut agent = (*snap.agents[n]).clone();
                            agent.id = format!("added-{i}");
                            agent.parent = None;
                            snap.agents.push(std::sync::Arc::new(agent));
                        }
                        10 => {
                            if snap.agents.len() > 1 {
                                snap.agents.remove(n);
                            }
                        }
                        11 => {
                            let parent = snap.agents[0].id.clone();
                            snap.agent_mut(n).parent = if i % 2 == 0 { Some(parent) } else { None };
                        }
                        12 => {
                            let count = snap.agents.len().div_ceil(100);
                            for j in 0..count {
                                let len = snap.agents.len();
                                let agent = snap.agent_mut((n + j) % len);
                                agent.context.used_tokens = Some((i * 91) as f64);
                                agent.context.max_tokens = Some(100_000.);
                                agent.events.push(Event {
                                    t: now + 5000.,
                                    cat: "you".into(),
                                    failed: i % 2 == 0,
                                });
                                agent.items.push(Item {
                                    kind: "command_execution".into(),
                                    updated_at: format!("item-{i}-{j}"),
                                    ..Default::default()
                                });
                            }
                        }
                        13 => {
                            let agent = snap.agent_mut(n);
                            if agent.requests.is_empty() {
                                agent.requests.push("synthetic".into());
                            } else {
                                agent.requests.clear();
                            }
                            agent.items.clear();
                            agent.now = None;
                        }
                        _ => {
                            // Reordering, including shifts of every cached row index.
                            snap.agents.rotate_left(n);
                        }
                    }
                    snap.at = now + 100.;
                    a.apply(snap.clone(), &Changed::Agents(vec![]));
                    b.replace(snap);
                }
            }
            let (w, h) = [(233, 55), (71, 37), (160, 44), (47, 13)][i / 20 % 4];
            compare(&a, &b, w, h, i);
            // Cache hits must be tested too; stateful Surface scroll is part of the result.
            compare(&a, &b, w, h, i);
        }
    }
}

#[test]
fn repaired_tree_sort_and_topology_sequences() {
    let demo: Snapshot =
        serde_json::from_str(include_str!("../../../checks/fixtures/demo.json")).unwrap();
    let mut snap = demo.clone();
    snap.agents = (0..96)
        .map(|i| {
            let mut agent = (*demo.agents[0]).clone();
            agent.id = format!("repair-{i}");
            agent.parent = (i % 8 != 0).then(|| format!("repair-{}", i - i % 8));
            agent.status = "completed".into();
            agent.requests.clear();
            std::sync::Arc::new(agent)
        })
        .collect();
    let mut cached = State::new(snap);
    cached.follow = false;
    let mut reference = cached.clone();
    reference.cache_enabled = false;
    let mut seed = 0x8a39dc1u64;
    for step in 0..192 {
        compare(&cached, &reference, 160, 44, step);
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let mut next = cached.snap.clone();
        let i = seed as usize % next.agents.len();
        let at = next.at;
        let agent = next.agent_mut(i);
        match step % 8 {
            0 => agent.updated = Some("2026-11-01T00:00:00.000Z".into()),
            1 => agent.updated = Some("invalid timestamp".into()),
            2 => agent.requests = vec!["synthetic approval".into()],
            3 => {
                agent.requests.clear();
                agent.status = "running".into();
            }
            4 => agent.parent = Some("repair-0".into()),
            5 => agent.parent = None,
            6 => agent.events.push(Event {
                t: at,
                cat: "shell".into(),
                failed: false,
            }),
            _ => agent.status = "completed".into(),
        }
        cached.apply(next.clone(), &Changed::Agents(vec![]));
        reference.replace(next);
        compare(&cached, &reference, 160, 44, step);
    }
    // Malformed topology must use the independent fallback, including duplicate IDs.
    for step in 192..196 {
        let mut next = cached.snap.clone();
        next.agent_mut(0).parent = Some("repair-1".into());
        next.agent_mut(1).parent = Some("repair-0".into());
        if step % 2 == 0 {
            next.agent_mut(2).id = "repair-1".into();
        } else {
            next.agent_mut(2).id = "repair-2".into();
        }
        cached.apply(next.clone(), &Changed::Everything);
        reference.replace(next);
        compare(&cached, &reference, 233, 55, step);
    }
}

#[test]
fn retained_pace_deadlines_and_ancestor_counts() {
    use t3_agents_core::model::Agent;
    let epoch = 1_794_484_800_000.;
    let mut snap = Snapshot {
        at: epoch,
        ..Snapshot::default()
    };
    snap.agents = (0..64)
        .map(|i| {
            std::sync::Arc::new(Agent {
                id: format!("pace-{i}"),
                parent: (i > 0).then(|| format!("pace-{}", (i - 1) / 2)),
                status: "running".into(),
                events: vec![Event {
                    t: epoch,
                    cat: "shell".into(),
                    failed: false,
                }],
                ..Agent::default()
            })
        })
        .collect();
    let mut cached = State::new(snap);
    cached.follow = false;
    let mut reference = cached.clone();
    reference.cache_enabled = false;
    for step in 0..160 {
        let mut next = cached.snap.clone();
        let i = (step * 17) % 64;
        let a = next.agent_mut(i);
        a.status = if step % 3 == 0 {
            "running"
        } else {
            "completed"
        }
        .into();
        if step % 7 == 0 {
            a.events[0].t += 100.;
        }
        if step % 19 == 0 {
            a.parent = None;
        }
        cached.apply(next.clone(), &Changed::Agents(vec![]));
        reference.replace(next);
        let boundary = epoch + if step % 2 == 0 { 120_000. } else { 480_000. };
        for at in [
            boundary.next_down(),
            boundary,
            boundary.next_up(),
            epoch,
            boundary,
        ] {
            cached.now = at;
            reference.now = at;
            compare(&cached, &reference, 233, 55, step);
            compare(&cached, &reference, 100, 40, step);
        }
    }
}

#[test]
fn structural_observation_arc_multiplicity() {
    let snap: Snapshot =
        serde_json::from_str(include_str!("../../../checks/fixtures/demo.json")).unwrap();
    let mut cached = State::new(snap);
    cached.follow = false;
    let mut reference = cached.clone();
    reference.cache_enabled = false;
    for step in 0..96 {
        let mut next = cached.snap.clone();
        next.at += 100.;
        match step % 6 {
            0 => next.agents.push(next.agents[0].clone()),
            1 => {
                next.agents.pop();
            }
            2 => next.agents.reverse(),
            3 => {
                let mut a = (*next.agents[0]).clone();
                a.id = format!("added-{step}");
                a.parent = None;
                next.agents.push(std::sync::Arc::new(a));
            }
            4 => {
                next.agents.pop();
            }
            _ => {
                next.agent_mut(1).parent = Some(next.agents[0].id.clone());
            }
        }
        cached.apply(next.clone(), &Changed::Agents(vec![]));
        reference.replace(next);
        compare(&cached, &reference, 233, 55, step);
        compare(&cached, &reference, 100, 40, step);
    }
}

#[test]
fn asks_keep_exact_and_estimated_counts_separate_across_refreshes() {
    use std::sync::Arc;
    use t3_agents_core::model::{Agent, AskCards, OpenAsks};

    let at = t3_agents_core::model::ms(Some("2026-11-18T18:00:00Z")).unwrap();
    let agents = (0..5)
        .map(|i| {
            Arc::new(Agent {
                id: format!("t{i}"),
                title: format!("Thread {i}"),
                status: if i == 0 { "running" } else { "completed" }.into(),
                updated: Some("2026-11-18T18:00:00Z".into()),
                ..Agent::default()
            })
        })
        .collect();
    let snap = Snapshot {
        at,
        agents,
        ..Snapshot::default()
    };
    let text = |line: &[t3_agents_core::terminal::Cell]| -> String {
        line.iter().map(|cell| cell.ch).collect()
    };
    let mut state = State::new(snap.clone());
    state.usage.clear();
    state.pick_at("t0", at);
    let mut next = snap.clone();
    next.agent_mut(0).estimated_asks = 2;
    state.apply(next.clone(), &Changed::Agents(vec!["t0".into()]));
    state.action("n");
    assert_eq!(state.notice.as_deref(), Some("Nobody is waiting on you"));
    state.notice = None;
    assert!(state.waits.is_empty());
    for (w, h) in [(60, 20), (80, 24), (120, 40)] {
        let (estimated, surface) = ui::draw_grid(&state, w, h, &state.palette);
        let mut healthy = state.clone();
        healthy.replace(snap.clone());
        let (healthy, _) = ui::draw_grid(&healthy, w, h, &state.palette);
        let top = surface.list.y as usize * w;
        assert_eq!(estimated.cells[..top], healthy.cells[..top], "tiles at {w}");
        assert_eq!(
            estimated.cells[(h - 1) * w..],
            healthy.cells[(h - 1) * w..],
            "footer at {w}"
        );
    }

    next.agent_mut(0).open_asks = OpenAsks {
        nonblocking: 1,
        cards: AskCards { link: 1, secret: 0 },
        ..OpenAsks::default()
    };
    next.agent_mut(1).open_asks.blocking = 1;
    next.agent_mut(1).asked = Some("2026-11-18T17:58:00Z".into());
    next.agent_mut(2).open_asks.cards.secret = 1;
    next.agent_mut(2).asked = Some("2026-11-18T17:57:00Z".into());
    state.replace(next.clone());
    state.pick_at("t4", at);
    for expected in ["t2", "t1", "t0", "t2"] {
        state.action("n");
        assert_eq!(state.selected.as_deref(), Some(expected));
    }
    state.pick_at("t4", at);
    let (overflow, _) = ui::draw_grid(&state, 60, 14, &state.palette);
    assert!(
        overflow
            .cells
            .chunks_exact(60)
            .map(text)
            .any(|line| line.contains("more") && line.contains("◇1"))
    );
    state.pick_at("t0", at);
    for (w, h, per) in [(60, 20, 1), (80, 24, 2), (120, 40, 2)] {
        let (grid, surface) = ui::draw_grid(&state, w, h, &state.palette);
        let hit = surface.hits.iter().find(|hit| hit.id == "t0").unwrap();
        assert_eq!(hit.rect.h, per);
        let row = &grid.cells[hit.rect.y as usize * w..][..w];
        assert_eq!(row[2].ch, '◉');
        assert_ne!(row[0].fg, state.palette.hot);
        let row_text = text(row);
        assert!(row_text.contains("~2"), "{row_text}");
        assert!(
            row_text.contains("◇1") || row_text.contains("◇ 1 question open"),
            "{row_text}"
        );
        assert!(
            row_text.contains("◇link") || row_text.contains("◇ link"),
            "{row_text}"
        );
        assert!(
            text(&grid.cells[surface.detail.y as usize * w..])
                .contains("~2 asks in an agent reply")
        );
        let rows = grid.cells.chunks_exact(w).map(text).collect::<Vec<_>>();
        assert!(rows.iter().any(|line| line.contains("◇2 working")));
        assert!(rows.iter().any(|line| line.contains("◇ a link to open in T3")));
        let (reference, reference_surface) = ui::draw_grid_reference(&state, w, h, &state.palette);
        assert_eq!(surface, reference_surface);
        assert_eq!(grid.cells, reference.cells);
    }

    // Same ids, order, time and attention membership. A count/type-only change
    // must invalidate the cached tiles and update the row without a structural refresh.
    next.agent_mut(0).open_asks = OpenAsks {
        blocking: 3,
        nonblocking: 4,
        cards: AskCards { link: 5, secret: 6 },
    };
    state.apply(next.clone(), &Changed::Agents(vec!["t0".into()]));
    let (grid, surface) = ui::draw_grid(&state, 60, 20, &state.palette);
    let hit = surface.hits.iter().find(|hit| hit.id == "t0").unwrap();
    let row = &grid.cells[hit.rect.y as usize * 60..][..60];
    assert_eq!(row[2].ch, '◆');
    assert_eq!(row[0].fg, state.palette.hot);
    assert!(text(row).contains("◆18 ~2"), "{}", text(row));
    let (reference, _) = ui::draw_grid_reference(&state, 60, 20, &state.palette);
    assert_eq!(grid.cells, reference.cells);
    let (detail, frame) = ui::draw_grid(&state, 120, 40, &state.palette);
    let lines = text(&detail.cells[frame.detail.y as usize * 120..]);
    for expected in [
        "3 blocking questions",
        "6 secrets to enter",
        "◆ 5 links to open",
        "4 questions open",
        "~2 asks in an agent reply",
    ] {
        assert!(lines.contains(expected), "{expected}");
    }

    state.apply(
        snap,
        &Changed::Agents(vec!["t0".into(), "t1".into(), "t2".into()]),
    );
    state.action("n");
    assert_eq!(state.notice.as_deref(), Some("Nobody is waiting on you"));
}

/// The header rows are reused while their key matches (`ui::TopKey`). Hold the clock still so
/// only the field under test changes, change each key field in turn (floats by a single bit,
/// including the sign of zero), and require the cached frame to equal the uncached one after
/// every change, at two sizes and after a resize.
#[test]
fn header_cache_follows_every_key_field() {
    let demo: Snapshot =
        serde_json::from_str(include_str!("../../../checks/fixtures/demo.json")).unwrap();
    let mut a = State::new(demo);
    a.follow = false;
    let mut b = a.clone();
    b.cache_enabled = false;
    let nudge = |x: f64| f64::from_bits(x.to_bits() ^ 1);
    let edits: Vec<(&str, Box<dyn Fn(&mut State)>)> = vec![
        ("nothing", Box::new(|_| {})),
        ("palette color", Box::new(|s| s.palette.accent ^= 0x010101)),
        ("palette floor", Box::new(move |s| s.palette.floor = nudge(s.palette.floor))),
        ("palette dark", Box::new(|s| s.palette.dark = !s.palette.dark)),
        ("calm", Box::new(|s| s.calm = !s.calm)),
        ("paused", Box::new(|s| s.paused = !s.paused)),
        ("error", Box::new(|s| s.error = Some("synthetic error".into()))),
        ("error cleared", Box::new(|s| s.error = None)),
        ("interval", Box::new(|s| s.interval = 5.)),
        ("demo", Box::new(|s| s.demo = !s.demo)),
        ("linked", Box::new(|s| s.linked = s.snap.agents.first().map(|a| a.id.clone()))),
        ("follow", Box::new(|s| s.follow = !s.follow)),
        ("scope", Box::new(|s| s.scope = "active".into())),
        ("scope back", Box::new(|s| s.scope = "all".into())),
        ("window", Box::new(|s| s.window_index += 1)),
        ("acked", Box::new(|s| {
            let id = s.snap.agents.iter().find(|a| t3_agents_core::model::needs_you(a)).or(s.snap.agents.first()).map(|a| a.id.clone()).unwrap();
            s.acked.insert(id);
        })),
        ("usage percent", Box::new(|s| {
            if let Some(l) = s.usage.iter_mut().flat_map(|p| p.limits.iter_mut()).next() {
                l.percent += 17.;
            }
        })),
        ("usage percent one bit", Box::new(move |s| {
            if let Some(l) = s.usage.iter_mut().flat_map(|p| p.limits.iter_mut()).next() {
                l.percent = nudge(l.percent);
            }
        })),
        ("usage today zero", Box::new(|s| {
            if let Some(p) = s.usage.first_mut() {
                p.today = 0.;
            }
        })),
        ("usage today negative zero", Box::new(|s| {
            if let Some(p) = s.usage.first_mut() {
                p.today = -0.;
            }
        })),
        ("usage stale", Box::new(|s| {
            if let Some(p) = s.usage.first_mut() {
                p.stale = !p.stale;
            }
        })),
        ("usage tier", Box::new(|s| {
            if let Some(p) = s.usage.first_mut() {
                p.tier = "synthetic tier".into();
            }
        })),
        ("usage model tokens", Box::new(|s| {
            if let Some(m) = s.usage.iter_mut().flat_map(|p| p.models.iter_mut()).next() {
                m.tokens += 1000.;
            }
        })),
        ("usage dropped", Box::new(|s| {
            s.usage.pop();
        })),
        ("now", Box::new(|s| s.now += 1000.)),
    ];
    let mut step = 0;
    for (w, h) in [(120, 40), (180, 55), (120, 40)] {
        for (name, edit) in &edits {
            edit(&mut a);
            edit(&mut b);
            // Twice: the second frame is drawn from whatever the first one cached.
            for _ in 0..2 {
                let (ca, sa) = ui::draw_grid(&a, w, h, &a.palette);
                let (cb, sb) = ui::draw_grid(&b, w, h, &b.palette);
                assert_eq!(ca.cells, cb.cells, "{name} at {w}x{h}, step {step}");
                assert_eq!(sa, sb, "{name} at {w}x{h}, step {step}");
                *a.surface.borrow_mut() = sa;
                *b.surface.borrow_mut() = sb;
                step += 1;
            }
        }
    }
}

/// The agent list's rows are copied from the previous frame while their key matches
/// (`ui::RowKey`). With motion on (spinner, blink, shimmer, tweens), run a scripted sequence:
/// the clock across every second, minute, hour and day boundary of each agent's age (each
/// boundary, the instant before and the instant after), spinner and blink beats, selection
/// moves, folding, scope and window changes, the help overlay, motion off and on, an acked
/// ask, a linked thread, a theme change and single-bit palette changes, snapshot changes, and
/// resizes between list and card layouts. Every frame must equal the uncached one, twice (the
/// second frame is drawn from whatever the first cached).
#[test]
fn list_row_cache_follows_the_clock_and_every_input() {
    use t3_agents_core::{integration, model::ms, theme};
    let demo: Snapshot =
        serde_json::from_str(include_str!("../../../checks/fixtures/demo.json")).unwrap();
    // Few enough rows for card layout (two lines a row) at the larger sizes.
    let mut small = demo.clone();
    small.agents.truncate(7);
    let sizes = [(120, 40), (180, 55), (233, 55), (100, 30), (71, 37), (160, 44)];
    let mut step = 0;
    for snap in [demo, small] {
        let mut a = State::new(snap);
        a.follow = false;
        a.calm = false;
        let mut b = a.clone();
        b.cache_enabled = false;
        let epoch = a.now;
        let mut size = 0;
        let frame = |a: &State, b: &State, size: usize, step: &mut usize| {
            let (w, h) = sizes[size % sizes.len()];
            compare(a, b, w, h, *step);
            compare(a, b, w, h, *step);
            *step += 1;
        };
        let both = |a: &mut State, b: &mut State, f: &dyn Fn(&mut State)| {
            f(a);
            f(b);
        };
        // Motion: 37 ms apart for 2.2 s crosses spinner beats (100 ms) and blink edges (550 ms).
        for i in 0..60 {
            let at = epoch + i as f64 * 37.;
            both(&mut a, &mut b, &|s| {
                s.advance_frame(at, false);
            });
            frame(&a, &b, size, &mut step);
        }
        // Every age boundary of every agent, in time order, at three sizes.
        let mut instants = Vec::new();
        for agent in &a.snap.agents {
            let Some(start) = ms(agent.started.as_deref()) else { continue };
            for secs in [1., 59., 60., 61., 599., 600., 601., 3599., 3600., 3601., 86399., 86400.] {
                let t = start + secs * 1000.;
                if t > epoch {
                    instants.push(t);
                }
            }
            // The two pace thresholds (quiet and stalled) after the agent's last event.
            if let Some(last) = agent.events.iter().map(|e| e.t).reduce(f64::max) {
                instants.extend([last + 120_000., last + 480_000.]);
            }
        }
        instants.sort_by(f64::total_cmp);
        instants.dedup();
        for (k, t) in instants.into_iter().enumerate() {
            for at in [t.next_down(), t, t.next_up()] {
                both(&mut a, &mut b, &|s| {
                    s.advance_frame(at, false);
                });
                frame(&a, &b, k / 7 % 3, &mut step);
            }
        }
        let now = a.now;
        let paper = integration::derive_themed(theme::builtin("paper").unwrap().text);
        let nudge = |x: f64| f64::from_bits(x.to_bits() ^ 1);
        let first_id = a.snap.agents[0].id.clone();
        let waiting_id = a
            .snap
            .agents
            .iter()
            .find(|x| t3_agents_core::model::needs_you(x))
            .map(|x| x.id.clone());
        let edits: Vec<(&str, Box<dyn Fn(&mut State)>)> = vec![
            ("down", Box::new(|s| s.action("down"))),
            ("down", Box::new(|s| s.action("down"))),
            ("up", Box::new(|s| s.action("up"))),
            ("end", Box::new(|s| s.action("end"))),
            ("home", Box::new(|s| s.action("home"))),
            ("pick", Box::new(move |s| s.pick_at(&first_id, now))),
            ("fold", Box::new(|s| s.action("left"))),
            ("unfold", Box::new(|s| s.action("right"))),
            ("space", Box::new(|s| s.action("space"))),
            ("scope", Box::new(|s| s.action("a"))),
            ("scope", Box::new(|s| s.action("a"))),
            ("scope", Box::new(|s| s.action("a"))),
            ("window", Box::new(|s| s.action("t"))),
            ("help", Box::new(|s| s.action("?"))),
            ("help scroll", Box::new(|s| s.action("down"))),
            ("help off", Box::new(|s| s.action("?"))),
            ("motion off", Box::new(|s| s.action("m"))),
            ("motion on", Box::new(|s| s.action("m"))),
            ("acked", Box::new(move |s| {
                if let Some(id) = &waiting_id {
                    s.acked.insert(id.clone());
                }
            })),
            ("linked", Box::new(|s| s.linked = s.snap.agents.get(1).map(|x| x.id.clone()))),
            ("home folder", Box::new(|s| s.home = "/home/synthetic".into())),
            ("theme", Box::new(move |s| s.palette = paper)),
            ("palette color", Box::new(|s| s.palette.sel ^= 0x010101)),
            ("palette floor", Box::new(move |s| s.palette.floor = nudge(s.palette.floor))),
            ("palette dark", Box::new(|s| s.palette.dark = !s.palette.dark)),
            ("tween", Box::new(|s| {
                if let Some((_, v)) = s.tween.iter_mut().next() {
                    *v = nudge(*v);
                }
            })),
        ];
        // Each edit is drawn at one size before and after, so the frame after it can reuse rows
        // cached before it; the size moves on between edits.
        for (i, (name, edit)) in edits.iter().enumerate() {
            let (w, h) = sizes[i % sizes.len()];
            compare(&a, &b, w, h, step);
            edit(&mut a);
            edit(&mut b);
            for _ in 0..3 {
                compare(&a, &b, w, h, step);
                let at = a.now + 41.;
                both(&mut a, &mut b, &|s| {
                    s.advance_frame(at, false);
                });
                step += 1;
            }
            let _ = name;
        }
        // Snapshot changes: status (starts a shimmer), context (starts a tween), a burst of
        // events (moves a lane and the lane maximum), a waiting request and a title; drawn across the animations they start.
        for i in 0..24 {
            let mut snap = a.snap.clone();
            // The burst goes to the first agent, a root that every scope shows.
            let n = if i % 5 == 2 { 0 } else { (i * 5) % snap.agents.len() };
            let at = a.now;
            let agent = snap.agent_mut(n);
            match i % 5 {
                0 => agent.status = if agent.status == "running" { "completed" } else { "running" }.into(),
                1 => {
                    agent.context.used_tokens = Some(91_000. + i as f64);
                    agent.context.max_tokens = Some(100_000.);
                }
                // A burst raises the busiest lane column, which rescales every other row's lane.
                2 => agent.events.extend((0..40).map(|k| Event {
                    t: at - k as f64,
                    cat: "shell".into(),
                    failed: i % 2 == 0 && k == 0,
                })),
                3 => agent.requests = vec!["synthetic".into()],
                _ => agent.title = format!("renamed {i}"),
            }
            snap.at = at;
            a.apply(snap.clone(), &Changed::Agents(vec![]));
            b.replace(snap);
            for k in 0..12 {
                let at = a.now + 53.;
                both(&mut a, &mut b, &|s| {
                    s.advance_frame(at, false);
                });
                size += usize::from(k == 11);
                frame(&a, &b, size, &mut step);
            }
        }
    }
}

/// Rows the sequence above does not reach, cached against uncached as there:
///
/// - a burst of events that raises the lane maximum every other row is scaled by;
/// - a chain deep enough that the context bar, age and lane draw over a running row's mark,
///   where the mark must stay in the key (the overlay check refuses those rows);
/// - a new tool call, which starts a name shimmer that must be drawn every frame;
/// - a folded parent whose child starts and stops waiting (the folded ◆);
/// - a home folder that occurs in the activity labels of card rows.
#[test]
fn list_row_cache_edge_rows() {
    use std::sync::Arc;
    use t3_agents_core::model::Agent;
    let demo: Snapshot =
        serde_json::from_str(include_str!("../../../checks/fixtures/demo.json")).unwrap();
    let epoch = demo.at;
    let run = |a: &mut State, b: &mut State, frames: usize, sizes: &[(usize, usize)], step: &mut usize| {
        for k in 0..frames {
            let at = a.now + 37.;
            a.advance_frame(at, false);
            b.advance_frame(at, false);
            let (w, h) = sizes[k % sizes.len()];
            compare(a, b, w, h, *step);
            compare(a, b, w, h, *step);
            *step += 1;
        }
    };
    let mut step = 0;

    // A chain of running agents, each the parent of the next.
    let deep = Snapshot {
        agents: (0..30)
            .map(|i| {
                Arc::new(Agent {
                    id: format!("deep-{i}"),
                    title: format!("deep {i}"),
                    parent: (i > 0).then(|| format!("deep-{}", i - 1)),
                    status: "running".into(),
                    started: Some("2026-11-12T11:58:00Z".into()),
                    events: vec![Event {
                        t: epoch - 1000.,
                        cat: "shell".into(),
                        failed: false,
                    }],
                    ..(*demo.agents[0]).clone()
                })
            })
            .collect(),
        ..demo.clone()
    };
    let mut a = State::new(deep);
    a.follow = false;
    a.calm = false;
    let mut b = a.clone();
    b.cache_enabled = false;
    for size in [(71, 37), (100, 60), (60, 70)] {
        // Same size for a while: the spinner turns while every other input holds.
        run(&mut a, &mut b, 30, &[size], &mut step);
    }

    let mut a = State::new(demo.clone());
    a.follow = false;
    a.calm = false;
    let mut b = a.clone();
    b.cache_enabled = false;
    let sizes = [(120, 40), (180, 55)];
    // Past the meters' tween, so rows are reused from here on.
    run(&mut a, &mut b, 40, &sizes[..1], &mut step);
    // A burst of events near the clock raises the busiest lane column: every other row's
    // lane rescales while its own columns stay the same.
    let mut snap = a.snap.clone();
    let at = a.now;
    snap.agent_mut(0).events.extend((0..40).map(|k| Event {
        t: at - k as f64,
        cat: "shell".into(),
        failed: false,
    }));
    snap.at = at;
    a.apply(snap.clone(), &Changed::Agents(vec![]));
    b.replace(snap);
    run(&mut a, &mut b, 6, &sizes[..1], &mut step);
    // A new tool call on a running agent: its name shimmers for 450 ms.
    for i in 0..3 {
        let mut snap = a.snap.clone();
        let n = snap.agents.iter().position(|x| x.status == "running" && x.requests.is_empty()).unwrap();
        snap.agent_mut(n).items.insert(
            0,
            Item {
                kind: "command_execution".into(),
                status: "running".into(),
                command: format!("make step-{i}"),
                updated_at: format!("shimmer-{i}"),
                ..Default::default()
            },
        );
        snap.at = a.now;
        a.apply(snap.clone(), &Changed::Agents(vec![]));
        b.replace(snap);
        run(&mut a, &mut b, 20, &sizes[i % 2..i % 2 + 1], &mut step);
    }
    // Fold every parent, then let one child of each wait and stop waiting.
    let parents: Vec<String> = a
        .snap
        .agents
        .iter()
        .filter_map(|x| x.parent.clone())
        .collect();
    for id in &parents {
        a.pick_at(id, a.now);
        b.pick_at(id, b.now);
        for key in ["space", "left"] {
            a.action(key);
            b.action(key);
            run(&mut a, &mut b, 2, &sizes, &mut step);
        }
    }
    for waiting in [true, false, true] {
        let mut snap = a.snap.clone();
        for n in 0..snap.agents.len() {
            if snap.agents[n].parent.is_some() {
                snap.agent_mut(n).requests = if waiting { vec!["synthetic".into()] } else { vec![] };
            }
        }
        snap.at = a.now;
        a.apply(snap.clone(), &Changed::Agents(vec![]));
        b.replace(snap);
        run(&mut a, &mut b, 6, &sizes[..1], &mut step);
    }

    // Card rows (few agents) show activity labels, which shorten the home folder.
    let mut small = demo;
    small.agents.truncate(7);
    let mut a = State::new(small);
    a.follow = false;
    a.calm = false;
    let mut b = a.clone();
    b.cache_enabled = false;
    // Let the context meters finish their tween first, or every row redraws anyway.
    run(&mut a, &mut b, 50, &[(180, 55)], &mut step);
    // Two commands start with these; `tidy` shortens a leading home folder to `~`.
    for home in ["", "npm", "cargo", ""] {
        run(&mut a, &mut b, 3, &[(180, 55)], &mut step);
        a.home = home.into();
        b.home = home.into();
        run(&mut a, &mut b, 3, &[(180, 55)], &mut step);
    }
}
