use chrono::{DateTime, Duration, NaiveTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;

#[cfg(test)]
mod simulation;

pub(super) const WINDOW_DAYS: i64 = 7;
pub(super) const MIN_MATURE_DAYS: f64 = 14.0;
pub(super) const MAX_DAYS: f64 = 56.0;
pub(super) const HARM_P: f64 = 0.99;
pub(super) const SRM_MIN_ASSIGNED: u64 = 100;
/// The upper 0.001 quantile of chi-square with 1 df, so SRM at p < 0.001 needs no CDF.
pub(super) const SRM_CHI2_P001: f64 = 10.827_566_170_662_733;
/// Picked by `simulation::calibration_grid`; docs/research/ab-testing.md §9 has the evidence.
pub(super) const RULE: Rule = Rule {
    floor: 0.996,
    epsilon: 0.02,
    min_converters: 100,
};

const WINDOW: Duration = Duration::days(WINDOW_DAYS);
const TAIL_TOLERANCE: f64 = 1e-15;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Rule {
    pub floor: f64,
    pub epsilon: f64,
    pub min_converters: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Pair<T> {
    pub control: T,
    pub variant: T,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Counts {
    pub assigned: u64,
    pub subjects: u64,
    pub converters: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Snapshot {
    pub counts: Pair<Counts>,
    pub weights: Pair<u32>,
    pub mature_days: f64,
    /// Real days since the start while running, for the duration limit; `None` once stopped.
    pub running_days: Option<f64>,
    /// Stopped, and every attribution window has closed.
    pub final_: bool,
    /// False once stopped for SRM: the split is broken, so no arm can win.
    pub trusted: bool,
}

/// An arm of a two-arm experiment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Control,
    Variant,
}

/// What the daily snapshot shows. It is recomputed every day, so a winner can disappear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ReadoutState {
    Collecting { need: Need },
    Winner { arm: Side },
    NoDifference { basis: Basis },
}

/// Why a readout is still collecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Need {
    /// Fewer than 14 days of mature assignments.
    Time,
    /// An arm has fewer converters than the rule needs.
    Converters,
    /// No arm is clearly ahead yet, or the split is broken.
    Evidence,
}

/// Why a readout shows no difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// Either arm loses less than the indifference margin in expectation.
    Equivalent,
    /// The experiment ended and no arm reached the floor.
    Ended,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ArmStats {
    pub p_best: f64,
    pub expected_loss: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Verdict {
    pub state: ReadoutState,
    pub arms: Pair<ArmStats>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Guardrail {
    Srm { chi2: f64 },
    Harm { p_worse: f64 },
    MaxDuration,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Window {
    pub as_of: DateTime<Utc>,
    pub decides_from: DateTime<Utc>,
    pub mature_days: f64,
    pub running_days: Option<f64>,
    pub final_: bool,
}

pub(super) fn window(
    started_at: DateTime<Utc>,
    stopped_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Window {
    let today = midnight(now);
    let as_of = stopped_at.map_or(today, |s| today.min(s + WINDOW));
    let mature_until = stopped_at.map_or(as_of - WINDOW, |s| (as_of - WINDOW).min(s));
    let decidable = started_at + WINDOW + Duration::days(MIN_MATURE_DAYS as i64);
    let decides_from = if midnight(decidable) < decidable {
        midnight(decidable) + Duration::days(1)
    } else {
        decidable
    };
    Window {
        as_of,
        decides_from,
        mature_days: days(mature_until - started_at).max(0.0),
        running_days: stopped_at.is_none().then(|| days(now - started_at)),
        final_: stopped_at.is_some_and(|s| today >= s + WINDOW),
    }
}

fn midnight(t: DateTime<Utc>) -> DateTime<Utc> {
    t.date_naive().and_time(NaiveTime::MIN).and_utc()
}

fn days(d: Duration) -> f64 {
    d.as_seconds_f64() / 86_400.0
}

pub(super) fn readout(s: &Snapshot) -> Verdict {
    let arms = posterior(&s.counts);
    Verdict {
        state: RULE.decide(s, &arms),
        arms,
    }
}

pub(super) fn guardrail(s: &Snapshot) -> Option<Guardrail> {
    guard(s, &posterior(&s.counts))
}

impl Rule {
    fn decide(&self, s: &Snapshot, arms: &Pair<ArmStats>) -> ReadoutState {
        use ReadoutState::{Collecting, NoDifference, Winner};
        let c = &s.counts;
        let margin = self.epsilon * Beta::of(c.control).mean();
        let (leader, p_leader) = if arms.variant.p_best > arms.control.p_best {
            (Side::Variant, arms.variant.p_best)
        } else {
            (Side::Control, arms.control.p_best)
        };
        [
            (
                !s.trusted,
                Collecting {
                    need: Need::Evidence,
                },
            ),
            (
                s.mature_days < MIN_MATURE_DAYS,
                Collecting { need: Need::Time },
            ),
            (
                c.control.converters.min(c.variant.converters) < self.min_converters,
                Collecting {
                    need: Need::Converters,
                },
            ),
            (
                arms.control.expected_loss < margin && arms.variant.expected_loss < margin,
                NoDifference {
                    basis: Basis::Equivalent,
                },
            ),
            (p_leader >= self.floor, Winner { arm: leader }),
            (
                s.final_,
                NoDifference {
                    basis: Basis::Ended,
                },
            ),
        ]
        .into_iter()
        .find_map(|(hit, state)| hit.then_some(state))
        .unwrap_or(Collecting {
            need: Need::Evidence,
        })
    }
}

fn guard(s: &Snapshot, arms: &Pair<ArmStats>) -> Option<Guardrail> {
    let running_days = s.running_days?;
    let assigned = s.counts.control.assigned + s.counts.variant.assigned;
    let chi2 = srm_chi2(s);
    let p_worse = arms.control.p_best;
    [
        (
            assigned >= SRM_MIN_ASSIGNED && chi2 > SRM_CHI2_P001,
            Guardrail::Srm { chi2 },
        ),
        (
            s.mature_days >= MIN_MATURE_DAYS && p_worse > HARM_P,
            Guardrail::Harm { p_worse },
        ),
        (running_days >= MAX_DAYS, Guardrail::MaxDuration),
    ]
    .into_iter()
    .find_map(|(hit, guardrail)| hit.then_some(guardrail))
}

/// Pearson's chi-square of all assignments against the configured weights.
fn srm_chi2(s: &Snapshot) -> f64 {
    let (c, w) = (&s.counts, &s.weights);
    let n = (c.control.assigned + c.variant.assigned) as f64;
    let total = f64::from(w.control) + f64::from(w.variant);
    let term = |observed: u64, weight: u32| {
        let expected = n * f64::from(weight) / total;
        let gap = observed as f64 - expected;
        gap * gap / expected
    };
    term(c.control.assigned, w.control) + term(c.variant.assigned, w.variant)
}

#[derive(Debug, Clone, Copy)]
struct Beta {
    a: u64,
    b: u64,
}

impl Beta {
    fn of(c: Counts) -> Self {
        Self {
            a: 1 + c.converters,
            b: 1 + c.subjects - c.converters,
        }
    }

    fn mean(self) -> f64 {
        self.a as f64 / (self.a + self.b) as f64
    }

    fn plus(self) -> Self {
        Self {
            a: self.a + 1,
            ..self
        }
    }
}

fn posterior(c: &Pair<Counts>) -> Pair<ArmStats> {
    let (control, variant) = (Beta::of(c.control), Beta::of(c.variant));
    let (p_control, p_variant) = exceed(control, variant);
    let (loss_control, loss_variant) = if variant.mean() > control.mean() {
        let (lead, trail) = losses(variant, control);
        (trail, lead)
    } else {
        losses(control, variant)
    };
    Pair {
        control: ArmStats {
            p_best: p_control,
            expected_loss: loss_control,
        },
        variant: ArmStats {
            p_best: p_variant,
            expected_loss: loss_variant,
        },
    }
}

/// The leader's loss `E[max(trail - lead, 0)]` comes from the small tails where the trailing arm
/// wins, so it never cancels two large numbers; the trailing loss adds the mean difference.
fn losses(lead: Beta, trail: Beta) -> (f64, f64) {
    let lead_loss = (trail.mean() * exceed(trail.plus(), lead).0
        - lead.mean() * exceed(trail, lead.plus()).0)
        .max(0.0);
    (lead_loss, lead_loss + lead.mean() - trail.mean())
}

fn exceed(x: Beta, y: Beta) -> (f64, f64) {
    exceed_within(x, y, TAIL_TOLERANCE)
}

/// `(P(X > Y), P(Y > X))`, each summed over its own tail of H ~ Hypergeometric(nx + ny, nx, k),
/// since X > Y exactly when H < x.a. The walk starts at the mode with weight 1 and stops a
/// direction once its remaining mass is certified below `tolerance` of the mass so far.
fn exceed_within(x: Beta, y: Beta, tolerance: f64) -> (f64, f64) {
    let (nx, ny, k) = (x.a + x.b - 1, y.a + y.b - 1, x.a + y.a - 1);
    let (lo, hi) = (k.saturating_sub(ny), k.min(nx));
    let mode = (u128::from(k + 1) * u128::from(nx + 1) / u128::from(nx + ny + 2)) as u64;
    let mode = mode.clamp(lo, hi);
    let (nxf, nyf, kf) = (nx as f64, ny as f64, k as f64);
    let up = |h: f64| (nxf - h) / (h + 1.0) * ((kf - h) / (nyf - kf + h + 1.0));
    let down = |h: f64| h / (nxf - h + 1.0) * ((nyf - kf + h) / (kf - h + 1.0));
    let mut tails = [Sum::default(); 2];
    tails[usize::from(mode >= x.a)].add(1.0);
    let mut mass = 1.0;

    let (mut h, mut w) = (mode, 1.0);
    while h < hi {
        let r = up(h as f64);
        if r < 1.0 && w * r / (1.0 - r) <= tolerance * mass {
            break;
        }
        (h, w) = (h + 1, w * r);
        mass += w;
        tails[usize::from(h >= x.a)].add(w);
    }
    let (mut h, mut w) = (mode, 1.0);
    while h > lo {
        let r = down(h as f64);
        if r < 1.0 && w * r / (1.0 - r) <= tolerance * mass {
            break;
        }
        (h, w) = (h - 1, w * r);
        mass += w;
        tails[usize::from(h >= x.a)].add(w);
    }
    let [below, above] = tails.map(Sum::value);
    (below / (below + above), above / (below + above))
}

/// Neumaier's compensated sum of non-negative terms.
#[derive(Debug, Clone, Copy, Default)]
struct Sum {
    sum: f64,
    carry: f64,
}

impl Sum {
    fn add(&mut self, x: f64) {
        let t = self.sum + x;
        self.carry += if self.sum >= x {
            (self.sum - t) + x
        } else {
            (x - t) + self.sum
        };
        self.sum = t;
    }

    fn value(self) -> f64 {
        self.sum + self.carry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arm(subjects: u64, converters: u64) -> Counts {
        Counts {
            assigned: subjects,
            subjects,
            converters,
        }
    }

    fn beta(a: u64, b: u64) -> Counts {
        arm(a + b - 2, a - 1)
    }

    fn snapshot(control: Counts, variant: Counts) -> Snapshot {
        Snapshot {
            counts: Pair { control, variant },
            weights: Pair {
                control: 50,
                variant: 50,
            },
            mature_days: 30.0,
            running_days: Some(37.0),
            final_: false,
            trusted: true,
        }
    }

    fn arms(control: Counts, variant: Counts) -> Pair<ArmStats> {
        readout(&snapshot(control, variant)).arms
    }

    #[track_caller]
    fn close(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "{actual:e} is not within {tolerance:e} of {expected:e}"
        );
    }

    fn stats(
        p_control: f64,
        loss_control: f64,
        p_variant: f64,
        loss_variant: f64,
    ) -> Pair<ArmStats> {
        Pair {
            control: ArmStats {
                p_best: p_control,
                expected_loss: loss_control,
            },
            variant: ArmStats {
                p_best: p_variant,
                expected_loss: loss_variant,
            },
        }
    }

    #[test]
    fn the_rule_applies_its_gates_in_order() {
        use ReadoutState::{Collecting, NoDifference, Winner};
        let rule = Rule {
            floor: 0.95,
            epsilon: 0.01,
            min_converters: 10,
        };
        let ready = snapshot(arm(10_000, 100), arm(10_000, 100));
        let converters = |control, variant| Snapshot {
            counts: Pair {
                control: arm(10_000, control),
                variant: arm(10_000, variant),
            },
            ..ready
        };
        let variant_ahead = stats(0.03, 0.002, 0.97, 0.000_01);
        let control_ahead = stats(0.96, 0.000_01, 0.04, 0.002);
        let within_margin = stats(0.97, 0.000_09, 0.03, 0.000_01);
        let unclear = stats(0.2, 0.000_02, 0.8, 0.000_2);
        let ended = Snapshot {
            final_: true,
            ..ready
        };
        for (s, arms, expected, why) in [
            (
                Snapshot {
                    trusted: false,
                    mature_days: 3.0,
                    ..ready
                },
                variant_ahead,
                Collecting {
                    need: Need::Evidence,
                },
                "a broken split outranks every other gate",
            ),
            (
                Snapshot {
                    mature_days: 13.99,
                    ..converters(9, 9)
                },
                variant_ahead,
                Collecting { need: Need::Time },
                "time comes before converters",
            ),
            (
                converters(9, 100),
                within_margin,
                Collecting {
                    need: Need::Converters,
                },
                "converters come before equivalence",
            ),
            (
                converters(100, 9),
                variant_ahead,
                Collecting {
                    need: Need::Converters,
                },
                "the variant needs the converters too",
            ),
            (
                ready,
                within_margin,
                NoDifference {
                    basis: Basis::Equivalent,
                },
                "equivalence comes before a winner",
            ),
            (
                ended,
                variant_ahead,
                Winner { arm: Side::Variant },
                "a winner comes before the end",
            ),
            (
                ready,
                control_ahead,
                Winner { arm: Side::Control },
                "control can win",
            ),
            (
                ended,
                unclear,
                NoDifference {
                    basis: Basis::Ended,
                },
                "a final readout without a winner has ended",
            ),
            (
                ready,
                unclear,
                Collecting {
                    need: Need::Evidence,
                },
                "otherwise it keeps collecting",
            ),
            (
                Snapshot {
                    mature_days: 14.0,
                    ..converters(10, 10)
                },
                stats(0.05, 0.002, 0.95, 0.000_01),
                Winner { arm: Side::Variant },
                "14 mature days, the converter minimum and the floor all count",
            ),
        ] {
            assert_eq!(rule.decide(&s, &arms), expected, "{why}");
        }
    }

    #[test]
    fn a_tied_leader_is_control() {
        let rule = Rule {
            floor: 0.5,
            epsilon: 0.0,
            min_converters: 0,
        };
        let s = snapshot(arm(10_000, 100), arm(10_000, 100));
        assert_eq!(
            rule.decide(&s, &stats(0.5, 0.01, 0.5, 0.01)),
            ReadoutState::Winner { arm: Side::Control }
        );
    }

    #[test]
    fn the_indifference_margin_is_relative_to_the_control_rate() {
        let rule = Rule {
            floor: 0.95,
            epsilon: 0.01,
            min_converters: 0,
        };
        let losses = stats(0.4, 0.000_5, 0.6, 0.000_5);
        assert_eq!(
            rule.decide(&snapshot(arm(10_000, 1000), arm(10_000, 1000)), &losses),
            ReadoutState::NoDifference {
                basis: Basis::Equivalent
            },
            "0.0005 is within 1% of a 10% rate"
        );
        assert_eq!(
            rule.decide(&snapshot(arm(10_000, 100), arm(10_000, 100)), &losses),
            ReadoutState::Collecting {
                need: Need::Evidence
            },
            "0.0005 is not within 1% of a 1% rate"
        );
    }

    #[test]
    fn the_readout_names_a_clear_winner_but_never_after_an_srm_stop() {
        let s = snapshot(arm(20_000, 400), arm(20_000, 600));
        assert_eq!(
            readout(&s).state,
            ReadoutState::Winner { arm: Side::Variant }
        );
        let srm = Snapshot {
            trusted: false,
            running_days: None,
            final_: true,
            ..s
        };
        assert_eq!(
            readout(&srm).state,
            ReadoutState::Collecting {
                need: Need::Evidence
            }
        );
    }

    fn assigned(control: u64, variant: u64) -> Snapshot {
        Snapshot {
            counts: Pair {
                control: Counts {
                    assigned: control,
                    ..Counts::default()
                },
                variant: Counts {
                    assigned: variant,
                    ..Counts::default()
                },
            },
            mature_days: 0.0,
            running_days: Some(1.0),
            ..snapshot(Counts::default(), Counts::default())
        }
    }

    #[test]
    fn srm_fires_above_the_p_0_001_cutoff_from_100_assignments() {
        assert_eq!(guardrail(&assigned(66, 34)), None, "66/34 gives chi2 10.24");
        let Some(Guardrail::Srm { chi2 }) = guardrail(&assigned(67, 33)) else {
            panic!("67/33 is an SRM");
        };
        close(chi2, 11.56, 1e-12);
        assert_eq!(
            guardrail(&assigned(99, 0)),
            None,
            "99 assignments never test"
        );
        let weighted = |control, variant| Snapshot {
            weights: Pair {
                control: 1,
                variant: 3,
            },
            ..assigned(control, variant)
        };
        assert_eq!(guardrail(&weighted(250, 750)), None, "250/750 fits 1:3");
        assert!(
            matches!(guardrail(&weighted(500, 500)), Some(Guardrail::Srm { .. })),
            "500/500 breaks 1:3"
        );
    }

    #[test]
    fn harm_needs_14_mature_days_but_no_converter_minimum() {
        let s = |mature_days| Snapshot {
            mature_days,
            ..snapshot(arm(1000, 20), arm(1000, 2))
        };
        assert_eq!(guardrail(&s(13.99)), None);
        let Some(Guardrail::Harm { p_worse }) = guardrail(&s(14.0)) else {
            panic!("2 converters against 20 is harm");
        };
        assert_eq!(p_worse, readout(&s(14.0)).arms.control.p_best);
        assert!(p_worse > HARM_P, "{p_worse}");
        let mild = Snapshot {
            mature_days: 30.0,
            ..snapshot(arm(10_000, 210), arm(10_000, 200))
        };
        assert_eq!(guardrail(&mild), None, "a mild control lead is no harm");
    }

    #[test]
    fn max_duration_stops_at_56_running_days() {
        let s = |running_days| Snapshot {
            running_days: Some(running_days),
            ..snapshot(arm(10_000, 200), arm(10_000, 200))
        };
        assert_eq!(guardrail(&s(55.99)), None);
        assert_eq!(guardrail(&s(56.0)), Some(Guardrail::MaxDuration));
    }

    #[test]
    fn guardrails_rank_srm_then_harm_then_max_duration_and_skip_a_stopped_experiment() {
        let harmful = Snapshot {
            running_days: Some(60.0),
            ..snapshot(arm(10_000, 300), arm(10_000, 200))
        };
        let mut broken = harmful;
        broken.counts.control.assigned = 20_000;
        assert!(
            matches!(guardrail(&broken), Some(Guardrail::Srm { .. })),
            "SRM outranks harm and the duration"
        );
        assert!(
            matches!(guardrail(&harmful), Some(Guardrail::Harm { .. })),
            "harm outranks the duration"
        );
        let stopped = Snapshot {
            running_days: None,
            ..broken
        };
        assert_eq!(
            guardrail(&stopped),
            None,
            "a stopped experiment is not guarded"
        );
    }

    fn at(t: &str) -> DateTime<Utc> {
        t.parse().unwrap()
    }

    #[test]
    fn a_running_window_ends_at_the_last_utc_midnight() {
        let w = window(at("2026-10-01T06:00:00Z"), None, at("2026-10-29T15:30:00Z"));
        assert_eq!(w.as_of, at("2026-10-29T00:00:00Z"));
        assert_eq!(w.mature_days, 20.75, "mature until 22 October, 00:00");
        close(w.running_days.unwrap(), 28.0 + 9.5 / 24.0, 1e-12);
        assert!(!w.final_);
        assert_eq!(
            w.decides_from,
            at("2026-10-23T00:00:00Z"),
            "the first midnight after the start plus 21 days"
        );
    }

    #[test]
    fn the_first_week_has_no_mature_days_and_day_21_decides() {
        let start = at("2026-10-01T00:00:00Z");
        assert_eq!(
            window(start, None, at("2026-10-05T12:00:00Z")).mature_days,
            0.0
        );
        let w = window(start, None, at("2026-10-22T00:00:00Z"));
        assert_eq!(w.mature_days, MIN_MATURE_DAYS);
        assert_eq!(w.decides_from, w.as_of, "start + 21 days is a midnight");
    }

    #[test]
    fn a_stop_caps_the_window_and_it_turns_final_seven_days_later() {
        let start = at("2026-10-01T00:00:00Z");
        let stop = at("2026-10-20T12:00:00Z");
        let w = window(start, Some(stop), at("2026-10-22T08:00:00Z"));
        assert_eq!(
            (w.as_of, w.mature_days, w.running_days, w.final_),
            (at("2026-10-22T00:00:00Z"), 14.0, None, false)
        );
        let w = window(start, Some(stop), at("2026-11-02T08:00:00Z"));
        assert_eq!(
            (w.as_of, w.mature_days, w.final_),
            (at("2026-10-27T12:00:00Z"), 19.5, true),
            "the snapshot stops at stop + 7 days and no subject after the stop matures"
        );
    }

    #[test]
    fn final_flips_exactly_at_stop_plus_seven_days() {
        let start = at("2026-10-01T00:00:00Z");
        let stop = at("2026-10-20T00:00:00Z");
        assert!(!window(start, Some(stop), at("2026-10-26T23:59:59Z")).final_);
        let w = window(start, Some(stop), at("2026-10-27T00:00:00Z"));
        assert_eq!(
            (w.as_of, w.mature_days, w.final_),
            (at("2026-10-27T00:00:00Z"), 19.0, true)
        );
    }

    #[test]
    fn readout_states_serialize_tagged_by_state() {
        for (state, json) in [
            (
                ReadoutState::Collecting {
                    need: Need::Converters,
                },
                serde_json::json!({"state": "collecting", "need": "converters"}),
            ),
            (
                ReadoutState::Winner { arm: Side::Variant },
                serde_json::json!({"state": "winner", "arm": "variant"}),
            ),
            (
                ReadoutState::NoDifference {
                    basis: Basis::Equivalent,
                },
                serde_json::json!({"state": "no_difference", "basis": "equivalent"}),
            ),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), json);
        }
    }

    #[test]
    fn the_readout_matches_exact_rational_posteriors() {
        for (control, variant, p_control, loss_control, loss_variant) in [
            (beta(1, 1), beta(1, 1), 1.0 / 2.0, 1.0 / 6.0, 1.0 / 6.0),
            (beta(2, 1), beta(1, 1), 2.0 / 3.0, 1.0 / 12.0, 1.0 / 4.0),
            (
                beta(3, 4),
                beta(2, 5),
                8.0 / 11.0,
                41.0 / 1001.0,
                184.0 / 1001.0,
            ),
            (
                beta(6, 6),
                beta(4, 3),
                165.0 / 442.0,
                809.0 / 6188.0,
                367.0 / 6188.0,
            ),
        ] {
            let a = arms(control, variant);
            close(a.control.p_best, p_control, 1e-15);
            close(a.variant.p_best, 1.0 - p_control, 1e-15);
            close(a.control.expected_loss, loss_control, 1e-15);
            close(a.variant.expected_loss, loss_variant, 1e-15);
        }
    }

    #[test]
    fn the_readout_matches_exact_rational_sums_at_shop_scale() {
        for (control, variant, p_variant, loss_control, loss_variant) in [
            (
                arm(100, 10),
                arm(100, 15),
                0.853_283_789_933_966_1,
                0.052_597_695_973_848_3,
                0.003_578_088_130_711_048,
            ),
            (
                arm(1000, 5),
                arm(1000, 25),
                0.999_913_788_275_847,
                0.019_960_199_106_560_626,
                1.192_662_412_655_891_2e-7,
            ),
            (
                arm(10_000, 200),
                arm(10_000, 240),
                0.973_013_850_477_564_7,
                0.004_020_624_298_052_02,
                2.142_413_808_401_393_3e-5,
            ),
            (
                arm(60_000, 1200),
                arm(60_000, 1300),
                0.978_358_455_179_466_9,
                0.001_673_232_053_083_074_4,
                6.620_940_120_173_22e-6,
            ),
        ] {
            let a = arms(control, variant);
            close(a.variant.p_best, p_variant, 1e-14);
            close(a.control.p_best, 1.0 - p_variant, 1e-14);
            close(a.control.expected_loss, loss_control, 1e-15);
            close(a.variant.expected_loss, loss_variant, 1e-15);
        }
    }

    #[test]
    fn a_tiny_chance_and_a_tiny_loss_keep_their_digits() {
        let a = arms(arm(50_000, 1000), arm(50_000, 1200));
        close(a.control.p_best, 8.034_457_957_603_618e-6, 1e-15);
        close(a.variant.expected_loss, 1.580_449_430_235_745e-9, 1e-15);
        close(a.control.expected_loss, 0.003_999_841_586_849_174, 1e-15);
    }

    #[test]
    fn swapping_the_arms_swaps_the_posterior() {
        let a = arms(arm(10_000, 200), arm(10_000, 240));
        let b = arms(arm(10_000, 240), arm(10_000, 200));
        close(a.control.p_best, b.variant.p_best, 1e-15);
        close(a.variant.p_best, b.control.p_best, 1e-15);
        close(a.control.expected_loss, b.variant.expected_loss, 1e-18);
        close(a.variant.expected_loss, b.control.expected_loss, 1e-18);
    }

    #[test]
    fn identical_arms_split_evenly_even_at_100k_converters() {
        let a = arms(arm(5_000, 100), arm(5_000, 100));
        close(a.control.p_best, 0.5, 1e-15);
        close(a.control.expected_loss, 0.001_120_662_542_347_292_9, 1e-15);
        close(a.variant.expected_loss, 0.001_120_662_542_347_292_9, 1e-15);

        let a = arms(arm(5_000_000, 100_000), arm(5_000_000, 100_000));
        close(a.control.p_best, 0.5, 1e-12);
        close(a.variant.p_best, 0.5, 1e-12);
        close(a.control.expected_loss, a.variant.expected_loss, 1e-15);
    }

    #[test]
    fn the_readout_stays_finite_and_matches_a_40_digit_sum_at_100k_converters_on_5m_subjects() {
        let a = arms(arm(5_000_000, 100_000), arm(5_000_000, 101_000));
        for v in [
            a.control.p_best,
            a.variant.p_best,
            a.control.expected_loss,
            a.variant.expected_loss,
        ] {
            assert!(v.is_finite() && v >= 0.0, "{a:?}");
        }
        close(a.variant.p_best, 0.987_878_526_882_486_4, 1e-14);
        close(a.control.p_best + a.variant.p_best, 1.0, 1e-15);
    }

    #[test]
    fn the_truncated_walk_matches_the_full_support() {
        for (x, y) in [
            (Beta { a: 201, b: 9801 }, Beta { a: 241, b: 9761 }),
            (Beta { a: 1201, b: 58_801 }, Beta { a: 1301, b: 58_701 }),
            (
                Beta {
                    a: 100_001,
                    b: 4_900_001,
                },
                Beta {
                    a: 101_001,
                    b: 4_899_001,
                },
            ),
        ] {
            let (above, below) = exceed(x, y);
            let (full_above, full_below) = exceed_within(x, y, 0.0);
            close(above, full_above, 1e-14);
            close(below, full_below, 1e-14);
        }
    }
}
