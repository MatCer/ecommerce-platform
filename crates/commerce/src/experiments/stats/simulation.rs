use std::time::Instant;

use super::{
    Basis, Counts, Guardrail, Pair, Phase, RULE, ReadoutState, Rule, Side, Snapshot, WINDOW_DAYS,
    guard, posterior,
};

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn uniform(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn coins(&mut self, n: u64) -> u64 {
        let words: u64 = (0..n / 64)
            .map(|_| u64::from(self.next().count_ones()))
            .sum();
        let rest = n % 64;
        let tail = if rest == 0 {
            0
        } else {
            u64::from((self.next() & ((1 << rest) - 1)).count_ones())
        };
        words + tail
    }

    fn binomial(&mut self, n: u64, p: f64) -> u64 {
        let u = self.uniform();
        let odds = p / (1.0 - p);
        let mut pmf = power(1.0 - p, n);
        let mut cdf = pmf;
        let mut k = 0;
        while cdf <= u && k < n {
            pmf *= (n - k) as f64 / (k + 1) as f64 * odds;
            k += 1;
            cdf += pmf;
        }
        k
    }

    fn cohort(&mut self, n: u64, p: f64) -> Counts {
        Counts {
            assigned: n,
            subjects: n,
            converters: self.binomial(n, p),
        }
    }
}

/// `base^exp` by squaring: Rust leaves `powi` and `powf` precision to the platform, and the
/// pinned tallies must not move with it.
fn power(mut base: f64, mut exp: u64) -> f64 {
    let mut acc = 1.0;
    while exp > 0 {
        if exp & 1 == 1 {
            acc *= base;
        }
        base *= base;
        exp >>= 1;
    }
    acc
}

#[derive(Debug, Clone, Copy)]
struct Scenario {
    daily: u64,
    base_bp: u64,
    lift_pct: u64,
    runs: u32,
}

impl Scenario {
    const fn aa(daily: u64, base_bp: u64) -> Self {
        Self {
            daily,
            base_bp,
            lift_pct: 0,
            runs: 4_000,
        }
    }

    const fn lift(daily: u64, lift_pct: u64) -> Self {
        Self {
            daily,
            base_bp: 200,
            lift_pct,
            runs: 2_000,
        }
    }

    fn seed(self) -> u64 {
        self.daily << 32 | self.base_bp << 16 | self.lift_pct
    }

    fn rates(self) -> Pair<f64> {
        let base = self.base_bp as f64 / 10_000.0;
        Pair {
            control: base,
            variant: base * (100 + self.lift_pct) as f64 / 100.0,
        }
    }
}

const AA: [Scenario; 5] = [
    Scenario::aa(100, 200),
    Scenario::aa(600, 200),
    Scenario::aa(1500, 200),
    Scenario::aa(600, 50),
    Scenario::aa(600, 500),
];
const SMB_POWER: Scenario = Scenario::lift(600, 20);

#[derive(Debug, Default, PartialEq, Eq)]
struct Stops {
    srm: u32,
    harm: u32,
    max_duration: u32,
}

#[derive(Debug, PartialEq)]
struct Outcome {
    runs_with_a_winner: u32,
    variant_winners: u32,
    median_first_win_day: Option<f64>,
    first_wins_after_stop: u32,
    equivalent_at_end: u32,
    equivalent_ever: u32,
}

#[derive(Default)]
struct Run {
    won: bool,
    first_variant_win: Option<u32>,
    equivalent: bool,
    last: Option<ReadoutState>,
}

#[derive(Default)]
struct Tally {
    runs_with_a_winner: u32,
    first_wins: Vec<u32>,
    first_wins_after_stop: u32,
    equivalent_at_end: u32,
    equivalent_ever: u32,
    run: Run,
}

const EQUIVALENT: ReadoutState = ReadoutState::NoDifference {
    basis: Basis::Equivalent,
};

impl Tally {
    fn peek(&mut self, day: u32, state: ReadoutState) {
        let run = &mut self.run;
        if let ReadoutState::Winner { arm } = state {
            run.won = true;
            if arm == Side::Variant {
                run.first_variant_win.get_or_insert(day);
            }
        }
        run.equivalent |= state == EQUIVALENT;
        run.last = Some(state);
    }

    fn end_run(&mut self, stop_day: u32) {
        let run = std::mem::take(&mut self.run);
        self.runs_with_a_winner += u32::from(run.won);
        if let Some(day) = run.first_variant_win {
            self.first_wins.push(day);
            self.first_wins_after_stop += u32::from(day > stop_day);
        }
        self.equivalent_ever += u32::from(run.equivalent);
        self.equivalent_at_end += u32::from(run.last == Some(EQUIVALENT));
    }

    fn outcome(mut self) -> Outcome {
        self.first_wins.sort_unstable();
        let n = self.first_wins.len();
        Outcome {
            runs_with_a_winner: self.runs_with_a_winner,
            variant_winners: n as u32,
            median_first_win_day: (n > 0)
                .then(|| f64::from(self.first_wins[(n - 1) / 2] + self.first_wins[n / 2]) / 2.0),
            first_wins_after_stop: self.first_wins_after_stop,
            equivalent_at_end: self.equivalent_at_end,
            equivalent_ever: self.equivalent_ever,
        }
    }
}

fn simulate(scenario: Scenario, rules: &[Rule]) -> (Stops, Vec<Outcome>) {
    let window = WINDOW_DAYS as u32;
    let rates = scenario.rates();
    let mut rng = SplitMix64(scenario.seed());
    let mut stops = Stops::default();
    let mut tallies: Vec<Tally> = rules.iter().map(|_| Tally::default()).collect();
    for _ in 0..scenario.runs {
        let mut cohorts: Vec<Pair<Counts>> = Vec::new();
        let mut counts = Pair::<Counts>::default();
        let mut mature_days = 0;
        let mut stop: Option<(u32, Guardrail)> = None;
        for day in 1_u32.. {
            if stop.is_none() {
                let control = rng.coins(scenario.daily);
                let cohort = Pair {
                    control: rng.cohort(control, rates.control),
                    variant: rng.cohort(scenario.daily - control, rates.variant),
                };
                counts.control.assigned += cohort.control.assigned;
                counts.variant.assigned += cohort.variant.assigned;
                cohorts.push(cohort);
            }
            if let Some(c) = day
                .checked_sub(window + 1)
                .and_then(|i| cohorts.get(i as usize))
            {
                for (total, cohort) in [
                    (&mut counts.control, c.control),
                    (&mut counts.variant, c.variant),
                ] {
                    total.subjects += cohort.subjects;
                    total.converters += cohort.converters;
                }
                mature_days += 1;
            }
            let snapshot = Snapshot {
                counts,
                weights: Pair {
                    control: 50,
                    variant: 50,
                },
                mature_days: f64::from(mature_days),
                phase: match stop {
                    None => Phase::Running {
                        days: f64::from(day),
                    },
                    Some((stop_day, _)) => Phase::Stopped {
                        final_: day == stop_day + window,
                    },
                },
                trusted: !matches!(stop, Some((_, Guardrail::Srm { .. }))),
            };
            let arms = posterior(&snapshot.counts);
            for (rule, tally) in rules.iter().zip(&mut tallies) {
                tally.peek(day, rule.decide(&snapshot, &arms));
            }
            if stop.is_none() {
                stop = guard(&snapshot, &arms).map(|g| (day, g));
            }
            if snapshot.phase.is_final() {
                break;
            }
        }
        let Some((stop_day, reason)) = stop else {
            unreachable!("the duration limit stops every run by day 56");
        };
        match reason {
            Guardrail::Srm { .. } => stops.srm += 1,
            Guardrail::Harm { .. } => stops.harm += 1,
            Guardrail::MaxDuration => stops.max_duration += 1,
        }
        for tally in &mut tallies {
            tally.end_run(stop_day);
        }
    }
    (stops, tallies.into_iter().map(Tally::outcome).collect())
}

fn wilson_upper(k: u32, n: u32) -> f64 {
    const Z: f64 = 1.959_963_984_540_054;
    let (k, n) = (f64::from(k), f64::from(n));
    let p = k / n;
    let z2 = Z * Z;
    (p + z2 / (2.0 * n) + Z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt()) / (1.0 + z2 / n)
}

fn share(k: u32, n: u32) -> f64 {
    f64::from(k) / f64::from(n)
}

#[test]
fn splitmix64_matches_its_reference_output() {
    assert_eq!(SplitMix64(0).next(), 0xe220_a839_7b1d_cdaf);
}

#[test]
fn the_samplers_match_their_binomial_moments() {
    let mut rng = SplitMix64(7);
    let coins: fn(&mut SplitMix64, u64, f64) -> u64 = |rng, n, _| rng.coins(n);
    for (n, p, sample) in [
        (1500, 0.5, coins),
        (65, 0.5, coins),
        (750, 0.02, SplitMix64::binomial),
        (300, 0.005, SplitMix64::binomial),
        (300, 0.05, SplitMix64::binomial),
    ] {
        let draws: Vec<f64> = (0..20_000).map(|_| sample(&mut rng, n, p) as f64).collect();
        let mean = draws.iter().sum::<f64>() / draws.len() as f64;
        let var = draws.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / (draws.len() - 1) as f64;
        let (want_mean, want_var) = (n as f64 * p, n as f64 * p * (1.0 - p));
        assert!(
            (mean - want_mean).abs() < 4.0 * (want_var / draws.len() as f64).sqrt(),
            "Binomial({n}, {p}) mean {mean}, want {want_mean}"
        );
        assert!(
            (var / want_var - 1.0).abs() < 0.05,
            "Binomial({n}, {p}) variance {var}, want {want_var}"
        );
    }
    assert_eq!(
        (rng.coins(0), rng.binomial(0, 0.3), rng.binomial(9, 0.0)),
        (0, 0, 0)
    );
}

const PINNED: [(Scenario, Stops, Outcome); 6] = [
    (
        AA[0],
        Stops {
            srm: 45,
            harm: 180,
            max_duration: 3775,
        },
        Outcome {
            runs_with_a_winner: 0,
            variant_winners: 0,
            median_first_win_day: None,
            first_wins_after_stop: 0,
            equivalent_at_end: 0,
            equivalent_ever: 0,
        },
    ),
    (
        AA[1],
        Stops {
            srm: 63,
            harm: 180,
            max_duration: 3757,
        },
        Outcome {
            runs_with_a_winner: 131,
            variant_winners: 73,
            median_first_win_day: Some(33.0),
            first_wins_after_stop: 4,
            equivalent_at_end: 0,
            equivalent_ever: 0,
        },
    ),
    (
        AA[2],
        Stops {
            srm: 62,
            harm: 187,
            max_duration: 3751,
        },
        Outcome {
            runs_with_a_winner: 148,
            variant_winners: 77,
            median_first_win_day: Some(29.0),
            first_wins_after_stop: 9,
            equivalent_at_end: 100,
            equivalent_ever: 228,
        },
    ),
    (
        AA[3],
        Stops {
            srm: 41,
            harm: 162,
            max_duration: 3797,
        },
        Outcome {
            runs_with_a_winner: 0,
            variant_winners: 0,
            median_first_win_day: None,
            first_wins_after_stop: 0,
            equivalent_at_end: 0,
            equivalent_ever: 0,
        },
    ),
    (
        AA[4],
        Stops {
            srm: 67,
            harm: 149,
            max_duration: 3784,
        },
        Outcome {
            runs_with_a_winner: 153,
            variant_winners: 85,
            median_first_win_day: Some(30.0),
            first_wins_after_stop: 9,
            equivalent_at_end: 156,
            equivalent_ever: 374,
        },
    ),
    (
        SMB_POWER,
        Stops {
            srm: 25,
            harm: 0,
            max_duration: 1975,
        },
        Outcome {
            runs_with_a_winner: 1056,
            variant_winners: 1056,
            median_first_win_day: Some(37.0),
            first_wins_after_stop: 123,
            equivalent_at_end: 0,
            equivalent_ever: 0,
        },
    ),
];

#[test]
fn the_stopping_rule_keeps_a_a_false_winners_under_five_percent() {
    let started = Instant::now();
    for (scenario, pinned_stops, pinned) in PINNED {
        let (stops, outcomes) = simulate(scenario, &[RULE]);
        let o = &outcomes[0];
        let runs = scenario.runs;
        if scenario.lift_pct == 0 {
            let upper = wilson_upper(o.runs_with_a_winner, runs);
            eprintln!(
                "A/A {}/day at {} bp: false winners {} of {runs} ({:.4}, Wilson upper {upper:.4}), \
                 SRM stops {:.4}, harm stops {:.4}",
                scenario.daily,
                scenario.base_bp,
                o.runs_with_a_winner,
                share(o.runs_with_a_winner, runs),
                share(stops.srm, runs),
                share(stops.harm, runs)
            );
            assert!(
                upper <= 0.05,
                "too many A/A false winners: {scenario:?} {o:?}"
            );
        } else {
            eprintln!(
                "+{}% at {}/day: power {:.4}, median first win day {:?}, first wins after the stop \
                 {} ({:.4} of runs, {:.4} of winners)",
                scenario.lift_pct,
                scenario.daily,
                share(o.variant_winners, runs),
                o.median_first_win_day,
                o.first_wins_after_stop,
                share(o.first_wins_after_stop, runs),
                share(o.first_wins_after_stop, o.variant_winners)
            );
        }
        assert_eq!(
            (&stops, o),
            (&pinned_stops, &pinned),
            "{scenario:?} moved: rerun calibration_grid and update docs/research/ab-testing.md §9"
        );
    }
    eprintln!("simulation took {:.2?}", started.elapsed());
}

const FLOORS: [f64; 7] = [0.99, 0.993, 0.995, 0.996, 0.997, 0.998, 0.999];
const EPSILONS: [f64; 4] = [0.005, 0.01, 0.02, 0.05];
const MIN_CONVERTERS: [u64; 4] = [0, 25, 50, 100];
const POWER: [Scenario; 3] = [Scenario::lift(300, 20), SMB_POWER, Scenario::lift(1200, 20)];
const SMALL_LIFT: Scenario = Scenario::lift(1500, 10);

#[test]
#[ignore = "calibration: run once in release; docs/research/ab-testing.md §9 records the output"]
fn calibration_grid() {
    let rules: Vec<Rule> = FLOORS
        .iter()
        .flat_map(|&floor| {
            EPSILONS.iter().flat_map(move |&epsilon| {
                MIN_CONVERTERS.iter().map(move |&min_converters| Rule {
                    floor,
                    epsilon,
                    min_converters,
                })
            })
        })
        .collect();
    let at = |f: usize, e: usize, m: usize| (f * EPSILONS.len() + e) * MIN_CONVERTERS.len() + m;
    let scenarios: Vec<Scenario> = AA.into_iter().chain(POWER).chain([SMALL_LIFT]).collect();
    let results: Vec<(Stops, Vec<Outcome>)> = std::thread::scope(|s| {
        let rules = &rules;
        let runs: Vec<_> = scenarios
            .iter()
            .map(|&scenario| s.spawn(move || simulate(scenario, rules)))
            .collect();
        runs.into_iter().map(|run| run.join().unwrap()).collect()
    });
    let (aa, rest) = results.split_at(AA.len());
    let (power, small_lift) = rest.split_at(POWER.len());
    let (smb, small_lift) = (&power[1].1, &small_lift[0].1);

    for (scenario, (stops, _)) in scenarios.iter().zip(&results) {
        println!(
            "stops {scenario:?}: SRM {:.4}, harm {:.4}, max duration {:.4}",
            share(stops.srm, scenario.runs),
            share(stops.harm, scenario.runs),
            share(stops.max_duration, scenario.runs)
        );
    }
    println!(
        "floor\tepsilon\tmin_converters\tA/A upper bound: {}\tpower: {}\t+10% equivalent at the end, ever",
        AA.map(|s| format!("{}/day {}bp", s.daily, s.base_bp))
            .join(" "),
        POWER.map(|s| format!("{}/day", s.daily)).join(" ")
    );
    for (i, rule) in rules.iter().enumerate() {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{:.4} {:.4}",
            rule.floor,
            rule.epsilon,
            rule.min_converters,
            aa.iter()
                .zip(AA)
                .map(|((_, o), s)| format!("{:.4}", wilson_upper(o[i].runs_with_a_winner, s.runs)))
                .collect::<Vec<_>>()
                .join(" "),
            power
                .iter()
                .zip(POWER)
                .map(|((_, o), s)| format!("{:.4}", share(o[i].variant_winners, s.runs)))
                .collect::<Vec<_>>()
                .join(" "),
            share(small_lift[i].equivalent_at_end, SMALL_LIFT.runs),
            share(small_lift[i].equivalent_ever, SMALL_LIFT.runs)
        );
    }

    let worst_upper = |f: usize| {
        (0..EPSILONS.len())
            .flat_map(|e| (0..MIN_CONVERTERS.len()).map(move |m| at(f, e, m)))
            .flat_map(|i| {
                aa.iter()
                    .zip(AA)
                    .map(move |((_, o), s)| wilson_upper(o[i].runs_with_a_winner, s.runs))
            })
            .fold(0.0, f64::max)
    };
    for (f, floor) in FLOORS.iter().enumerate() {
        println!(
            "criterion 1: floor {floor}: worst A/A upper bound {:.4}",
            worst_upper(f)
        );
    }
    let f = (0..FLOORS.len())
        .find(|&f| worst_upper(f) <= 0.05)
        .expect("no floor keeps every A/A cell under 5%");

    let cost = |e: usize, m: usize| {
        share(smb[at(f, e, 0)].variant_winners, SMB_POWER.runs)
            - share(smb[at(f, e, m)].variant_winners, SMB_POWER.runs)
    };
    let worst_cost = |m: usize| (0..EPSILONS.len()).map(|e| cost(e, m)).fold(0.0, f64::max);
    for (m, min) in MIN_CONVERTERS.iter().enumerate() {
        println!(
            "criterion 2: min_converters {min}: worst +20% power cost at 600/day {:.4}",
            worst_cost(m)
        );
    }
    let m = (0..MIN_CONVERTERS.len())
        .rev()
        .find(|&m| worst_cost(m) <= 0.01)
        .expect("min_converters 0 costs no power");

    let equivalent = |e: usize| share(small_lift[at(f, e, m)].equivalent_at_end, SMALL_LIFT.runs);
    for (e, epsilon) in EPSILONS.iter().enumerate() {
        println!(
            "criterion 3: epsilon {epsilon}: +10% at 1,500/day equivalent at the end {:.4}, ever {:.4}",
            equivalent(e),
            share(small_lift[at(f, e, m)].equivalent_ever, SMALL_LIFT.runs)
        );
    }
    let e = (0..EPSILONS.len())
        .rev()
        .find(|&e| equivalent(e) <= 0.05)
        .expect("even the smallest epsilon calls a +10% lift equivalent too often");

    let chosen = Rule {
        floor: FLOORS[f],
        epsilon: EPSILONS[e],
        min_converters: MIN_CONVERTERS[m],
    };
    println!("chosen {chosen:?}");
    for (scenario, (_, outcomes)) in scenarios.iter().zip(&results) {
        let o = &outcomes[at(f, e, m)];
        println!(
            "chosen {scenario:?}: winners {:.4} (upper {:.4}), variant winners {:.4}, median first \
             win day {:?}, first wins after the stop {:.4} of runs, {o:?}",
            share(o.runs_with_a_winner, scenario.runs),
            wilson_upper(o.runs_with_a_winner, scenario.runs),
            share(o.variant_winners, scenario.runs),
            o.median_first_win_day,
            share(o.first_wins_after_stop, scenario.runs)
        );
    }
    assert_eq!(chosen, RULE, "set RULE to the calibrated rule");
}
