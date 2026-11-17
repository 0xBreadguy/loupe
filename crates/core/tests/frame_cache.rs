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
