//! Live dashboard telemetry alert, headroom priority, help visibility, and CLI flag parsing.
//! Uses purely synthetic inputs and never reads host `~/.t3` or `/etc/t3code`.

use loupe_core::{
    headroom::{Volume, warning_volumes},
    model::{Snapshot, State},
    product::{self, Command, Environment},
    telemetry::{self, demo},
    terminal::Palette,
    ui,
};
use std::sync::Arc;

fn synth_state() -> State {
    let mut s = State::new(Snapshot::default());
    s.demo = false;
    s.themes = true;
    s.storage_key_in_help = true;
    s.palette = Palette::fallback();
    s
}

#[test]
fn telemetry_alert_shows_only_for_broken_opt_out_and_yields_to_low_space() {
    let base = synth_state();
    let (base_canvas, _) = ui::draw_grid(&base, 100, 30, &base.palette);

    // 1. Misconfigured telemetry report -> shows warning row with `e telemetry` hint
    let mut mis = synth_state();
    mis.telemetry = Some(Arc::new(demo::report(1_795_532_700_000.0)));
    let (mis_canvas, _) = ui::draw_grid(&mis, 100, 30, &mis.palette);
    let mis_text = mis_canvas.to_text();
    assert!(
        mis_text.contains("Telemetry opt-out not working") || mis_text.contains("Opt-out not working"),
        "expected telemetry warning line:\n{mis_text}"
    );
    assert!(mis_text.contains("e telemetry"), "expected e telemetry hint:\n{mis_text}");

    // 2. Under LOUPE_NO_THEMES (themes=false) -> byte-identical to baseline, no alert
    let mut no_themes = synth_state();
    no_themes.themes = false;
    no_themes.telemetry = mis.telemetry.clone();
    let (nt_canvas, _) = ui::draw_grid(&no_themes, 100, 30, &no_themes.palette);
    let mut base_nt = synth_state();
    base_nt.themes = false;
    let (base_nt_canvas, _) = ui::draw_grid(&base_nt, 100, 30, &base_nt.palette);
    assert_eq!(nt_canvas.to_text(), base_nt_canvas.to_text());

    // 3. In --demo mode -> alert stays silent on Live (so demo frames/screenshots stay clean)
    let mut in_demo = synth_state();
    in_demo.demo = true;
    in_demo.telemetry = mis.telemetry.clone();
    let (demo_canvas, _) = ui::draw_grid(&in_demo, 100, 30, &in_demo.palette);
    assert!(!demo_canvas.to_text().contains("e telemetry"));

    // 4. Low disk space WINS over telemetry alert
    let mut both = mis.clone();
    let gib = 1024 * 1024 * 1024u64;
    both.headroom = warning_volumes(
        [(
            Volume {
                device: 1,
                available_bytes: gib / 2,
                total_bytes: 100 * gib,
                is_tmpfs: Some(false),
            },
            "/".into(),
        )],
        None,
    );
    let (both_canvas, _) = ui::draw_grid(&both, 100, 30, &both.palette);
    let both_text = both_canvas.to_text();
    assert!(both_text.contains("s storage"), "low space must win:\n{both_text}");
    assert!(!both_text.contains("e telemetry"));

    // 5. Plain default-on telemetry -> NO alert, matches baseline
    let mut on_inputs = demo::inputs(1_795_532_700_000.0);
    on_inputs.shell = telemetry::EnvSet::default();
    on_inputs.hosts = telemetry::Read::Absent;
    on_inputs.runtime = telemetry::Read::Absent;
    on_inputs.server.liveness = telemetry::Liveness::Stopped;
    let on_rep = telemetry::evaluate(on_inputs);
    assert_eq!(on_rep.verdict, telemetry::Verdict::On);
    assert!(on_rep.alert().is_none());

    let mut plain_on = synth_state();
    plain_on.telemetry = Some(Arc::new(on_rep));
    let (plain_canvas, _) = ui::draw_grid(&plain_on, 100, 30, &plain_on.palette);
    assert_eq!(plain_canvas.to_text(), base_canvas.to_text());
}

#[test]
fn help_lists_e_only_when_product_views_enabled() {
    let mut s = synth_state();
    s.help = true;
    let (c, _) = ui::draw_grid(&s, 120, 44, &s.palette);
    assert!(c.to_text().contains("telemetry: what T3 sends"));

    let mut oracle_mode = synth_state();
    oracle_mode.themes = false;
    oracle_mode.storage_key_in_help = false;
    oracle_mode.help = true;
    let (oc, _) = ui::draw_grid(&oracle_mode, 120, 44, &oracle_mode.palette);
    assert!(!oc.to_text().contains("telemetry: what T3 sends"));
}

#[test]
fn cli_flag_parsing_and_no_themes_gating() {
    let env_on = Environment {
        themes: true,
        ..Environment::default()
    };
    let env_off = Environment {
        themes: false,
        ..Environment::default()
    };
    let s = |xs: &[&str]| xs.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();

    assert!(matches!(
        product::parse_in(&s(&["--telemetry-check", "--demo", "--json"]), &env_on),
        Ok(Command::TelemetryCheck {
            demo: true,
            json: true,
            ..
        })
    ));
    assert!(matches!(
        product::parse_in(&s(&["--telemetry-check", "--wide"]), &env_on),
        Ok(Command::TelemetryCheck {
            size: Some(loupe_core::recap_view::Size::Wide),
            ..
        })
    ));
    assert!(product::parse_in(&s(&["--telemetry-check", "--db", "/tmp/a.db"]), &env_on)
        .unwrap_err()
        .0
        .contains("leave out --db"));
    assert!(product::parse_in(&s(&["--telemetry-check", "--recap"]), &env_on).is_err());
    assert!(product::parse_in(&s(&["--telemetry-check", "--wide", "--narrow"]), &env_on).is_err());
    assert!(product::parse_in(&s(&["--telemetry-check"]), &env_off)
        .unwrap_err()
        .0
        .contains("Unknown option"));
}
