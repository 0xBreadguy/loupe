//! A Rust port of the input generators in `checks/scripts/text-parity.ts`.
//!
//! The original gate builds about 5.1 million inputs from a 32-bit linear congruential generator,
//! a pool of awkward characters and a few hundred hand-written boundary values. This module builds
//! the same inputs, in the same groups and the same order, without Node. Each input is rendered as
//! the exact line the gate pipes to the probe: `JSON.stringify(wire(input))`, where `wire` turns
//! every number that is not a safe integer (and `-0`, NaN and the infinities) into the string
//! JavaScript would print for it.
//!
//! The port is checked, not trusted: the recording holds a digest of each group's exact input
//! bytes, and `golden_text.rs` fails with the group's name if this module builds anything else.
//! The order in which random numbers are drawn matters as much as the formulas, so each
//! generator draws in the order the TypeScript expression evaluates. Where that order is easy to
//! get wrong a comment says so.
#![allow(clippy::type_complexity)]
use super::tables::*;

// ── the generator and JavaScript's number printing ───────────────────────────

/// `makeRandom(seed)`: `seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0`.
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        Rng(seed)
    }
    pub fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        self.next() as usize % n
    }
    /// `random() / 4294967296`
    fn unit(&mut self) -> f64 {
        self.next() as f64 / 4294967296.0
    }
}

/// ECMAScript `Number::toString`, for the values `wire` has to spell as strings.
pub fn js_number(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x == 0.0 {
        return "0".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if x < 0.0 {
        return format!("-{}", js_number(-x));
    }
    // `{:e}` prints the shortest digits that round-trip, as `d.ddde-7`. When several digit
    // strings of that length round-trip, ECMAScript takes the one closest to x (the even one on
    // an exact tie, which Rust's shortest printer does not guarantee), and the exact value rounded
    // to that many digits is that closest string.
    let shortest = format!("{x:e}");
    let places = shortest.split_once('e').unwrap().0.chars().filter(|c| *c != '.').count() - 1;
    let closest = format!("{x:.places$e}");
    let text = if closest.parse::<f64>().ok() == Some(x) { closest } else { shortest };
    let (mantissa, exponent) = text.split_once('e').unwrap();
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exponent.parse::<i32>().unwrap() + 1;
    if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let sign = if n - 1 < 0 { '-' } else { '+' };
        let power = (n - 1).abs();
        if k == 1 {
            format!("{digits}e{sign}{power}")
        } else {
            format!("{}.{}e{sign}{power}", &digits[..1], &digits[1..])
        }
    }
}

/// `10 ** k` as V8 computes it. It is the correctly rounded `1e{k}` everywhere the gate uses it
/// except `10 ** 23`, which V8 prints as 1.0000000000000001e+23.
fn pow10(k: i32) -> f64 {
    if k == 23 {
        1.0000000000000001e23
    } else {
        format!("1e{k}").parse().unwrap()
    }
}

// ── JSON as `JSON.stringify(wire(value))` writes it ──────────────────────────

pub enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(&'static str, J)>),
}

fn s(text: &str) -> J {
    J::Str(text.to_string())
}
fn n(value: impl Into<f64>) -> J {
    J::Num(value.into())
}
fn opt(text: Option<&str>) -> J {
    text.map_or(J::Null, s)
}

impl J {
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
            J::Num(x) => {
                // `wire`: safe integers other than -0 stay numbers; everything else is a string.
                let safe = x.is_finite() && x.fract() == 0.0 && x.abs() <= 9007199254740991.0 && !(*x == 0.0 && x.is_sign_negative());
                if safe {
                    out.push_str(&format!("{}", *x as i64));
                } else {
                    out.push_str(&serde_json::to_string(&js_number(*x)).unwrap());
                }
            }
            // serde_json escapes exactly what JSON.stringify does for well-formed strings.
            J::Str(text) => out.push_str(&serde_json::to_string(text).unwrap()),
            J::Arr(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            J::Obj(members) => {
                out.push('{');
                for (index, (key, value)) in members.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key).unwrap());
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

fn line(args: Vec<J>) -> String {
    J::Arr(args).render()
}

// ── shared pools ─────────────────────────────────────────────────────────────

fn random_strings(count: usize, seed: u32) -> Vec<String> {
    let mut random = Rng::new(seed);
    let mut strings = Vec::with_capacity(count);
    for _ in 0..count {
        let length = LENGTHS[random.below(LENGTHS.len())];
        let mut text = String::new();
        for _ in 0..length {
            text.push_str(POOL[random.below(POOL.len())]);
        }
        strings.push(text);
    }
    strings
}

/// ` ` to `~`.
fn alphabet() -> Vec<String> {
    (0x20u8..0x7f).map(|b| (b as char).to_string()).collect()
}

fn env_pool() -> Vec<String> {
    let mut pool = alphabet();
    pool.extend(["\u{a0}", "\u{feff}", "\u{85}", "中", "🍞", "\u{0}", "\u{7f}", ""].map(String::from));
    pool
}

/// `pickString(random, pool, maxLength)`: a length first, then one pool entry per character.
fn pick_string(random: &mut Rng, pool: &[String], max_length: usize) -> String {
    let length = random.below(max_length + 1);
    let mut text = String::new();
    for _ in 0..length {
        text.push_str(&pool[random.below(pool.len())]);
    }
    text
}

fn owned(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

/// `Date.UTC(year, month0, day, hour, minute, second, ms)` for dates in range.
fn utc(year: i64, month0: i64, day: i64, hour: i64, minute: i64, second: i64, ms: i64) -> f64 {
    // Days from civil (Howard Hinnant's algorithm), month 1..=12.
    let (y, m) = if month0 + 1 <= 2 { (year - 1, month0 + 1 + 9) } else { (year, month0 + 1 - 3) };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    (days * 86_400_000 + hour * 3_600_000 + minute * 60_000 + second * 1000 + ms) as f64
}

// ── the groups, in the order the gate runs them ──────────────────────────────

/// What the recording says about the oracle's own filtering (see `recordKept` in
/// `text-record.ts`): per label, one flag per candidate input, `None` when all were kept.
pub type Kept<'a> = &'a dyn Fn(&str) -> Option<Vec<bool>>;

/// Calls `emit(name, lines)` once per `compare(...)` call of the gate, in the gate's order.
pub fn groups(kept: Kept, emit: &mut dyn FnMut(&str, Vec<String>)) {
    let strings = random_strings(20000, 0x51ed5eed);
    let alphabet = alphabet();

    // 1. fitting and wrapping
    for max in 0..=60u32 {
        emit("fit", strings.iter().map(|text| line(vec![s(text), n(max)])).collect());
        let flags = kept(&format!("wrap max={max}"));
        let wrapping: Vec<String> = strings
            .iter()
            .enumerate()
            .filter(|(index, _)| flags.as_ref().is_none_or(|flags| flags[*index]))
            .map(|(_, text)| line(vec![s(text), n(max)]))
            .collect();
        emit("wrap", wrapping);
    }
    let width_vectors = ["中", "a中b", "\u{301}a", "😀😀", "🇺🇸x", "a\tb", "a\u{85}b", "a\u{feff}b", "\u{200b}"];
    let vector_flags = kept("wrap vectors");
    for (which, text) in width_vectors.iter().enumerate() {
        for max in 0..=4u32 {
            emit("fit", vec![line(vec![s(text), n(max)])]);
            if vector_flags.as_ref().is_none_or(|flags| flags[which * 5 + max as usize]) {
                emit("wrap", vec![line(vec![s(text), n(max)])]);
            }
        }
    }

    // 2. compact numbers and durations
    let numbers = number_sweep(20000, 0x0ddba11);
    let number_lines: Vec<String> = numbers.iter().map(|x| line(vec![n(*x)])).collect();
    for name in ["number_string", "tokens", "duration", "span"] {
        emit(name, number_lines.clone());
    }
    emit("tokens", vec![line(vec![J::Null])]);


    // Local time. The gate runs under several TZ values; timestamps stay inside 1969..2027.
    const HOUR: f64 = 3_600_000.0;
    let mut clock_times: Vec<f64> = vec![
        0.0, -1.0, 1.0, -60000.0, -60000.5, -59999.5, -1000.0, 999.0, -86400000.0, 86399999.0,
        utc(2024, 1, 29, 12, 0, 0, 0),
        utc(2025, 11, 31, 23, 59, 59, 999),
        utc(2026, 0, 1, 0, 0, 0, 1),
    ];
    // US spring forward 2026-03-08, US fall back 2026-11-01, EU spring 2026-03-29, EU fall 2026-10-25.
    for (year, month0, day, hour) in [
        (2026, 2, 7, 0), (2026, 2, 8, 6), (2026, 2, 9, 12),
        (2026, 9, 31, 0), (2026, 10, 1, 6), (2026, 10, 2, 12),
        (2026, 2, 28, 22), (2026, 2, 29, 4), (2026, 2, 30, 6),
        (2026, 9, 24, 20), (2026, 9, 25, 4), (2026, 9, 26, 6),
        (2025, 11, 31, 18), (2026, 0, 1, 6),
    ] {
        let anchor = utc(year, month0, day, hour, 0, 0, 0);
        for k in -8..=8 {
            clock_times.push(anchor + k as f64 * HOUR / 2.0);
        }
    }
    let mut clock_inputs = Vec::new();
    for t in &clock_times {
        for day in [*t, t + HOUR, t - 86_400_000.0, t + 86_400_000.0, 0.0] {
            clock_inputs.push(line(vec![n(*t), n(day)]));
        }
    }
    emit("clock", clock_inputs);
    let mut when_inputs = Vec::new();
    for (index, t) in clock_times.iter().enumerate() {
        if index % 2 == 1 {
            continue;
        }
        for end in [None, Some(t + 5.0 * 60000.0), Some(t + 2.0 * HOUR)] {
            when_inputs.push(line(vec![n(*t), end.map_or(J::Null, n), n(t + 10.0 * 60000.0)]));
        }
        when_inputs.push(line(vec![n(*t), n(t + 60000.0), n(t + 86_400_000.0 * 2.0)]));
    }
    emit("when", when_inputs);

    // 3. model names and families
    let mut model_inputs: Vec<String> = MODEL_VECTORS.iter().map(|text| line(vec![s(text)])).collect();
    model_inputs.extend(strings.iter().map(|text| line(vec![s(text)])));
    emit("short_model", model_inputs);

    let mut family_inputs: Vec<String> = FAMILY_VECTORS.iter().map(|(model, hint)| line(vec![opt(*model), opt(*hint)])).collect();
    {
        let mut random = Rng::new(0xfa111);
        for i in 0..20000usize {
            // `strings[(i * 7 + random()) % strings.length]`, then the hint's draws.
            let model = &strings[(i * 7 + random.next() as usize) % strings.len()];
            let hint = if random.next() % 3 == 0 {
                let lead = POOL[random.below(POOL.len())];
                let tail = if random.next() % 2 != 0 { "claude" } else { "gpt" };
                Some(format!("{lead}{tail}"))
            } else {
                None
            };
            family_inputs.push(line(vec![s(model), opt(hint.as_deref())]));
        }
    }
    emit("family", family_inputs);

    let mut category_inputs: Vec<String> = Vec::new();
    for kind in CATEGORY_KINDS {
        category_inputs.push(line(vec![s(kind), J::Null]));
    }
    for tool in CATEGORY_TOOLS {
        category_inputs.push(line(vec![s("dynamic_tool"), opt(*tool)]));
    }
    {
        let mut random = Rng::new(0xca7e90);
        for i in 0..20000usize {
            let kind = if random.next() % 5 == 0 { strings[(i * 3) % strings.len()].as_str() } else { "dynamic_tool" };
            let tool = &strings[(i * 11 + random.next() as usize) % strings.len()];
            let tool = if random.next() % 6 == 0 { J::Null } else { s(tool) };
            category_inputs.push(line(vec![s(kind), tool]));
        }
    }
    emit("category", category_inputs);

    // 4. identity and links. encodeURIComponent over every code point but the surrogates.
    let mut start = 0u32;
    while start < 0x110000 {
        let end = (start + 200000).min(0x110000);
        let inputs: Vec<String> = (start..end)
            .filter(|cp| !(0xd800..=0xdfff).contains(cp))
            .map(|cp| line(vec![s(&char::from_u32(cp).unwrap().to_string())]))
            .collect();
        emit("encode_uri_component", inputs);
        start += 200000;
    }
    emit("encode_uri_component", strings.iter().map(|text| line(vec![s(text)])).collect());
    for text in ["", " ", "!", "*", "'", "(", ")", "~", "-", "_", ".", "%", "/", "🍞", "a b"] {
        emit("encode_uri_component", vec![line(vec![s(text)])]);
    }

    let env_pool = env_pool();
    let bread = "🍞";
    let mut link_inputs: Vec<(String, String)> = vec![
        ("".into(), "".into()),
        ("e-1".into(), "x".into()),
        ("e-1".into(), "thread:task:a%3Ab/c".into()),
        ("日本".into(), "🍞/!".into()),
        ("e".into(), bread.repeat(512)),
        ("e".into(), bread.repeat(513)),
        ("e".into(), format!("{}a", bread.repeat(512))),
        ("e".into(), "a".repeat(1024)),
        ("e".into(), "a".repeat(1025)),
        ("e".repeat(128), "x".into()),
        ("e".repeat(129), "x".into()),
        ("a b".into(), "c d".into()),
        ("\u{0}".into(), "\u{1}".into()),
    ];
    {
        let mut random = Rng::new(0x11a5c);
        for _ in 0..20000 {
            let env = pick_string(&mut random, &env_pool, 20);
            let id = pick_string(&mut random, &env_pool, 30);
            link_inputs.push((env, id));
        }
        let short = owned(&["a", "🍞", "中"]);
        for _ in 0..200 {
            let env = pick_string(&mut random, &env_pool, 5);
            let id = pick_string(&mut random, &short, 600);
            link_inputs.push((env, id));
        }
    }
    let link_lines: Vec<String> = link_inputs.iter().map(|(env, id)| line(vec![s(env), s(id)])).collect();
    emit("thread_link", link_lines.clone());
    emit("thread_key", link_lines);
    emit("environment_id_valid", join_pool(20000, 0x1d5).iter().map(|text| line(vec![s(text)])).collect());

    emit("thread_ref", ref_inputs(&env_pool).into_iter().map(|value| line(vec![value])).collect());

    let mut title_inputs: Vec<String> = vec![line(vec![s(""), J::Arr(vec![])]), line(vec![s("Fix the build"), J::Arr(vec![])])];
    {
        let mut random = Rng::new(0x7171e);
        for _ in 0..8000 {
            let count = 1 + random.below(4);
            let agents: Vec<Agent> = (0..count).map(|k| make_agent(&mut random, k)).collect();
            let chosen = &agents[random.below(agents.len())];
            let title = match random.next() % 3 {
                0 => chosen.title.to_string(),
                1 => format!("{} / {}", chosen.project, chosen.title),
                _ => format!("{}{}", POOL[random.below(POOL.len())], chosen.title),
            };
            let title = dirty(&mut random, &title);
            title_inputs.push(line(vec![s(&title), J::Arr(agents.iter().map(Agent::json).collect())]));
        }
    }
    emit("thread_for_title", title_inputs);

    let mut agent_link_inputs = Vec::new();
    {
        let mut random = Rng::new(0xa9e17);
        for i in 0..4000 {
            let agent = make_agent(&mut random, i);
            let local = if random.next() % 4 == 0 { None } else { Some(pick_string(&mut random, &env_pool, 10)) };
            agent_link_inputs.push(line(vec![agent.json(), opt(local.as_deref())]));
        }
    }
    emit("agent_thread_link", agent_link_inputs);

    // 5. delegation ordering
    {
        let mut random = Rng::new(0xc011a7e);
        let mut timestamps = Vec::with_capacity(4000);
        for _ in 0..4000 {
            let y = 2024 + random.next() % 4;
            let mo = 1 + random.next() % 12;
            let d = 1 + random.next() % 28;
            let h = random.next() % 24;
            let mi = random.next() % 60;
            let se = random.next() % 60;
            let ms = random.next() % 1000;
            timestamps.push(format!("{y}-{mo:02}-{d:02}T{h:02}:{mi:02}:{se:02}.{ms:03}Z"));
        }
        let mut pairs: Vec<(String, String)> = Vec::new();
        for _ in 0..20000 {
            let (a, b);
            match random.next() % 4 {
                0 => {
                    a = timestamps[random.below(timestamps.len())].clone();
                    b = timestamps[random.below(timestamps.len())].clone();
                }
                1 => {
                    let base = timestamps[random.below(timestamps.len())].clone();
                    let cut = random.below(base.len());
                    a = base[..cut].to_string();
                    b = base;
                }
                _ => {
                    // `for (let j = 0; j < length(); j++)` draws a fresh length on every test.
                    let build = |random: &mut Rng| {
                        let mut text = String::new();
                        let mut j = 0;
                        while j < random.below(12) {
                            text.push_str(&alphabet[random.below(alphabet.len())]);
                            j += 1;
                        }
                        text
                    };
                    a = build(&mut random);
                    b = build(&mut random);
                }
            }
            pairs.push((a, b));
        }
        for (a, b) in [("a", "B"), ("a-b", "a_b"), ("a", "A"), ("A", "a"), ("aA", "Aa"), ("a", "ab"), ("", "a"), ("a", "")] {
            pairs.push((a.into(), b.into()));
        }
        emit("locale_compare", pairs.iter().map(|(a, b)| line(vec![s(a), s(b)])).collect());
    }

    // 6. remaining string and number helpers
    let mut window_inputs: Vec<String> = WINDOW_VECTORS.iter().map(|text| text.to_string()).collect();
    {
        let mut random = Rng::new(0x1d0b);
        let numbers = ["0", "1", "5", "12", "999", "123456789"];
        let gaps = ["", " ", "\t", "\u{a0}", "\u{feff}", "\u{85}", "-", " - ", " -", "-  "];
        let units = ["hour", "Hour", "HOUR", "hours", "day", "DAYS", "week", "WEEKLY", "month", "Monthly", "x"];
        for i in 0..20000usize {
            if i % 2 == 1 {
                window_inputs.push(strings[i % strings.len()].clone());
            } else {
                // The array literal builds its last entry (`'0'.repeat(1 + random() % 40)`)
                // before the index is drawn.
                let zeros = "0".repeat(1 + random.below(40));
                let which = random.below(7);
                let number = if which < 6 { numbers[which].to_string() } else { zeros };
                let gap = gaps[random.below(9)];
                let unit = units[random.below(11)];
                let before = POOL[random.below(POOL.len())];
                let after = POOL[random.below(POOL.len())];
                window_inputs.push(format!("{before}{number}{gap}{unit}{after}"));
            }
        }
    }
    emit("window_length", window_inputs.iter().map(|text| line(vec![s(text)])).collect());

    let mut hex_inputs: Vec<String> = HEX_VECTORS.iter().map(|text| text.to_string()).collect();
    {
        let mut random = Rng::new(0x4e6);
        let chars: Vec<char> = "0123456789abcdefABCDEFgG #\t\u{feff}é中".chars().collect();
        for _ in 0..20000 {
            let length = random.below(12);
            let mut text = if random.next() % 2 != 0 { "#".to_string() } else { String::new() };
            for _ in 0..length {
                text.push(chars[random.below(chars.len())]);
            }
            hex_inputs.push(text);
        }
    }
    emit("hex", hex_inputs.iter().map(|text| line(vec![s(text)])).collect());

    let mut colors: Vec<u32> = vec![0, 1, 0x010203, 0xffffff, 0x7aa2f7, 0xde6145, 0x101315, 0x808080];
    let mut random = Rng::new(0xc0105);
    while colors.len() < 12000 {
        colors.push(random.next() & 0xffffff);
    }
    let color_pairs: Vec<(u32, u32, f64)> = colors
        .iter()
        .map(|c| (*c, colors[(*c as usize * 7919) % colors.len()], (*c % 1001) as f64 / 1000.0))
        .collect();
    emit("mix", color_pairs.iter().map(|(a, b, t)| line(vec![n(*a), n(*b), n(*t)])).collect());
    emit("luminance", colors.iter().map(|c| line(vec![n(*c)])).collect());
    emit("contrast", color_pairs.iter().map(|(a, b, _)| line(vec![n(*a), n(*b)])).collect());
    emit("chroma", colors.iter().map(|c| line(vec![n(*c)])).collect());

    let mut ramp_inputs: Vec<String> = [-1.0, -0.5, -0.0, 0.0, 0.01, 0.54, 0.55, 0.56, 0.99, 1.0, 1.5, 2.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY]
        .iter()
        .map(|t| line(vec![n(*t)]))
        .collect();
    for _ in 0..20000 {
        ramp_inputs.push(line(vec![n(random.unit() * 3.0 - 1.0)]));
    }
    emit("ramp", ramp_inputs);

    let surfaces: [&[u32]; 5] = [&[0x101315, 0x171a1c], &[0x1a1b26], &[0x101315, 0x171a1c, 0x33467c], &[0xffffff], &[]];
    let surface_json = |index: usize| J::Arr(surfaces[index].iter().map(|c| n(*c)).collect());
    let mut reach_inputs: Vec<String> = Vec::new();
    {
        let mut random = Rng::new(0x4eac4);
        for target in [0.0, 1.0, 2.4, 3.0, 4.5, 7.0, 21.0] {
            for (from, to) in [(0x565d60u32, 0xffffffu32), (0xcacccc, 0xffffff), (0xde6145, 0x000000), (0x8b5896, 0xffffff)] {
                reach_inputs.push(line(vec![n(from), n(to), n(target), surface_json(0)]));
            }
        }
        while reach_inputs.len() < 12000 {
            let from = random.next() & 0xffffff;
            let to = random.next() & 0xffffff;
            let target = (random.next() % 2100) as f64 / 100.0;
            let pool = random.below(surfaces.len());
            reach_inputs.push(line(vec![n(from), n(to), n(target), surface_json(pool)]));
        }
    }
    emit("reach", reach_inputs);

    let mut level_inputs: Vec<String> = Vec::new();
    for v in [0.0, -0.0, 0.001, 0.1, 0.5, 1.0, 2.0, 7.0, 8.0, 1e9, -1e9, f64::INFINITY, f64::NEG_INFINITY] {
        for max in [0.0, 1.0, 2.0, 8.0, 100.0, 1e9, -1.0, -100.0] {
            level_inputs.push(line(vec![n(v), n(max)]));
        }
    }
    for _ in 0..20000 {
        let v = random.unit() * 100.0 - 50.0;
        let max = random.unit() * 1000.0;
        level_inputs.push(line(vec![n(v), n(max)]));
    }
    emit("level", level_inputs);

    let mut spark_inputs: Vec<String> = vec![line(vec![J::Arr(vec![]), J::Null])];
    for _ in 0..5000 {
        let length = random.below(9);
        let values: Vec<J> = (0..length).map(|_| n(random.unit() * 200.0 - 20.0)).collect();
        let max = if random.next() % 3 == 0 { J::Null } else { n(random.unit() * 300.0 - 10.0) };
        spark_inputs.push(line(vec![J::Arr(values), max]));
    }
    emit("spark", spark_inputs);

    let mut big_inputs: Vec<String> = Vec::new();
    for text in ["", "0", "1.2K", "99%", "--", "abc", "中", "🍞", ".", "-", "1 2"] {
        big_inputs.push(line(vec![s(text), J::Bool(false)]));
        big_inputs.push(line(vec![s(text), J::Bool(true)]));
    }
    for text in &strings {
        big_inputs.push(line(vec![s(text), J::Bool(false)]));
        big_inputs.push(line(vec![s(text), J::Bool(true)]));
    }
    emit("big_width", big_inputs);
}

// ── generators with their own helpers ────────────────────────────────────────

fn number_sweep(count: usize, seed: u32) -> Vec<f64> {
    let mut random = Rng::new(seed);
    let mut values: Vec<f64> = NUMBER_BOUNDARIES.to_vec();
    while values.len() < count {
        let kind = random.next() % 7;
        let unit = random.unit();
        values.push(match kind {
            0 => (random.next() % 2000001) as f64 - 1000000.0,
            1 => ((random.next() % 2000000) as f64 - 1000000.0) / 1000.0,
            2 => unit * pow10((random.next() % 41) as i32 - 10),
            3 => -unit * pow10((random.next() % 41) as i32 - 10),
            4 => (unit * pow10((random.next() % 25) as i32)).floor(),
            5 => unit * 1e6 - 5e5,
            _ => unit * 86400.0 * 2.0,
        });
    }
    values
}

fn join_pool(count: usize, seed: u32) -> Vec<String> {
    let mut random = Rng::new(seed);
    let alphabet = alphabet();
    let mut wide = alphabet.clone();
    wide.extend(["é", "中", "\u{feff}", "\u{85}"].map(String::from));
    let mut inputs: Vec<String> = vec![
        "".into(), "e".into(), "e-1".into(), "e_1".into(), ".".into(), "/".into(), "a b".into(), "é".into(), "中".into(),
        "a".repeat(128), "a".repeat(129), "\u{feff}e".into(), "e\u{85}".into(),
    ];
    while inputs.len() < count {
        let pool = if random.next() % 2 != 0 { &wide } else { &alphabet };
        inputs.push(pick_string(&mut random, pool, 140));
    }
    inputs
}

fn ref_inputs(env_pool: &[String]) -> Vec<J> {
    let obj = |env: J, thread: Option<J>| {
        let mut members = vec![("environmentId", env)];
        if let Some(thread) = thread {
            members.push(("threadId", thread));
        }
        J::Obj(members)
    };
    let bread = "🍞";
    let mut inputs: Vec<J> = vec![
        J::Null, n(0), n(1), J::Bool(true), J::Bool(false), s("x"), s(" {} "), J::Arr(vec![]), J::Arr(vec![n(1)]), J::Obj(vec![]),
        obj(s("e"), Some(s("t"))), obj(s(""), Some(s("t"))),
        obj(s("e"), Some(s(""))), obj(s("e"), Some(s("   "))),
        obj(s("e"), Some(s("\u{feff}"))), obj(s("e"), Some(s("\u{85}"))),
        obj(s("e"), Some(s(" \u{85} "))), obj(s("e"), Some(s("a\u{0}b"))),
        obj(s("e"), Some(s("a\nb"))), obj(s("e"), Some(s("a\u{7f}b"))),
        obj(s("e"), Some(s(&"a".repeat(1024)))), obj(s("e"), Some(s(&"a".repeat(1025)))),
        obj(s("e"), Some(s(&bread.repeat(512)))), obj(s("e"), Some(s(&format!("{}a", bread.repeat(512))))),
        obj(s(&"a".repeat(128)), Some(s("t"))), obj(s(&"a".repeat(129)), Some(s("t"))),
        obj(s("a b"), Some(s("t"))), obj(s("é"), Some(s("t"))),
        obj(n(1), Some(s("t"))), obj(s("e"), Some(n(1))),
        obj(J::Null, Some(s("t"))), obj(s("e"), Some(J::Null)),
        J::Obj(vec![("environmentId", s("e")), ("threadId", s("x")), ("extra", n(1))]),
    ];
    let mut random = Rng::new(0x7ef5);
    let envs = owned(&["", "e", "e-1"]).into_iter().chain([
        "a".repeat(128), "a".repeat(129), "a b".into(), "é".into(), "中".into(), "\u{feff}".into(), "e_1-2".into(),
    ]).collect::<Vec<String>>();
    let small = owned(&["a", "🍞", "中", " ", "\u{85}", "\u{feff}"]);
    for _ in 0..20000 {
        // The condition is drawn first, then the environment, then (in one branch) the thread.
        let bare = random.next() % 8 == 0;
        let env = envs[random.below(envs.len())].clone();
        let mut thread: Option<String> = None;
        if !bare {
            thread = Some(pick_string(&mut random, env_pool, 40));
        }
        if random.next() % 11 == 0 {
            // The third argument (`random() % 60`) is drawn before `pickString` draws its own.
            let longest = random.below(60);
            thread = Some(pick_string(&mut random, &small, longest));
        }
        inputs.push(obj(s(&env), thread.as_deref().map(s)));
    }
    inputs
}

pub struct Agent {
    id: String,
    thread_id: String,
    pub title: &'static str,
    pub project: &'static str,
    updated: String,
    remote: Option<(&'static str, &'static str)>,
}

impl Agent {
    fn json(&self) -> J {
        J::Obj(vec![
            ("id", s(&self.id)),
            ("thread_id", s(&self.thread_id)),
            ("title", s(self.title)),
            ("project", s(self.project)),
            ("updated", s(&self.updated)),
            ("remote", match self.remote {
                Some((environment, thread)) => J::Obj(vec![("environmentId", s(environment)), ("threadId", s(thread))]),
                None => J::Null,
            }),
        ])
    }
}

fn make_agent(random: &mut Rng, index: usize) -> Agent {
    let thread_id = if random.next() % 3 == 0 { format!("sub-{index}") } else { format!("id-{index}") };
    let id = format!("id-{index}");
    let remote = if random.next() % 4 == 0 {
        let environment = if random.next() % 5 != 0 { "machine-b" } else { "../bad" };
        let thread = if random.next() % 5 != 0 { "parent" } else { " " };
        Some((environment, thread))
    } else {
        None
    };
    // Object literal members are evaluated in the order they are written.
    let thread = if random.next() % 2 != 0 { id.clone() } else { thread_id };
    let title = AGENT_TITLES[random.below(AGENT_TITLES.len())];
    let project = AGENT_PROJECTS[random.below(AGENT_PROJECTS.len())];
    let updated = if random.next() % 4 == 0 {
        AGENT_UPDATED[random.below(AGENT_UPDATED.len())].to_string()
    } else {
        format!("2026-11-{}", 10 + random.next() % 9)
    };
    Agent { id, thread_id: thread, title, project, updated, remote }
}

fn dirty(random: &mut Rng, text: &str) -> String {
    let wrap = ["\u{0}", "\u{a0}", "\u{feff}", " ", "\u{85}", "\t", "\u{200b}", "\u{2028}"][random.below(8)];
    if random.next() % 3 == 0 {
        return text.to_string();
    }
    if random.next() % 2 != 0 {
        let mut out = String::new();
        for ch in text.chars() {
            if random.next() % 4 == 0 {
                out.push_str(wrap);
            }
            out.push(ch);
        }
        return out;
    }
    format!("{wrap}{text}{wrap}")
}
