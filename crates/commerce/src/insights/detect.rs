use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

pub mod rules {
    pub const FUNNEL_RECENT_DAYS: u64 = 7;
    pub const FUNNEL_BASELINE_DAYS: u64 = 21;
    pub const FUNNEL_MIN_FROM: i64 = 100;
    pub const FUNNEL_MAX_RATIO: f64 = 0.8;
    pub const FUNNEL_MIN_Z: f64 = 2.0;
    pub const PRODUCT_DAYS: u64 = 14;
    pub const PRODUCT_MIN_VIEWS: i64 = 200;
    pub const SHOP_MIN_VIEWS: i64 = 1000;
    pub const PRODUCT_MAX_RATIO: f64 = 0.5;
    pub const VITALS_DAYS: u64 = 14;
    pub const VITALS_MIN_SAMPLES: i64 = 50;
    pub const SLOW_PAGE_WEIGHT: f64 = 0.05;
}

use rules::*;

/// Adjacent funnel steps: sessions, view_item, add_to_cart, begin_checkout, purchase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Transition {
    SessionsToViewItem,
    ViewItemToAddToCart,
    AddToCartToBeginCheckout,
    BeginCheckoutToPurchase,
}

impl Transition {
    const ALL: [Self; 4] = [
        Self::SessionsToViewItem,
        Self::ViewItemToAddToCart,
        Self::AddToCartToBeginCheckout,
        Self::BeginCheckoutToPurchase,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionsToViewItem => "sessions_to_view_item",
            Self::ViewItemToAddToCart => "view_item_to_add_to_cart",
            Self::AddToCartToBeginCheckout => "add_to_cart_to_begin_checkout",
            Self::BeginCheckoutToPurchase => "begin_checkout_to_purchase",
        }
    }
}

/// Core Web Vitals with their "good" bound (ms; CLS unitless).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum Vital {
    #[serde(rename = "LCP")]
    Lcp,
    #[serde(rename = "INP")]
    Inp,
    #[serde(rename = "CLS")]
    Cls,
}

impl Vital {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "LCP" => Some(Self::Lcp),
            "INP" => Some(Self::Inp),
            "CLS" => Some(Self::Cls),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lcp => "LCP",
            Self::Inp => "INP",
            Self::Cls => "CLS",
        }
    }

    pub fn good(self) -> f64 {
        match self {
            Self::Lcp => 2500.0,
            Self::Inp => 200.0,
            Self::Cls => 0.1,
        }
    }
}

/// Sessions that reached the from-step, and of them the to-step, in one window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Pair {
    pub from: i64,
    pub to: i64,
}

pub(crate) fn rate(part: i64, whole: i64) -> f64 {
    part as f64 / whole as f64
}

/// What a detector found, with every number behind the score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Detection {
    /// The last 7 days against the 21 days before.
    FunnelDrop {
        transition: Transition,
        recent: Pair,
        baseline: Pair,
    },
    /// Views and add-to-carts over 14 days, of the product and of the whole shop.
    LowAddToCart {
        product_id: Uuid,
        views: i64,
        adds: i64,
        shop_views: i64,
        shop_adds: i64,
    },
    /// RUM p75 over 14 days and the template's page requests in that time.
    SlowPage {
        template: String,
        metric: Vital,
        p75: f64,
        samples: i64,
        requests: i64,
    },
}

impl Detection {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::FunnelDrop { .. } => "funnel_drop",
            Self::LowAddToCart { .. } => "low_add_to_cart",
            Self::SlowPage { .. } => "slow_page",
        }
    }

    pub fn subject(&self) -> String {
        match self {
            Self::FunnelDrop { transition, .. } => transition.as_str().to_owned(),
            Self::LowAddToCart { product_id, .. } => product_id.to_string(),
            Self::SlowPage {
                template, metric, ..
            } => format!("{template}:{}", metric.as_str()),
        }
    }

    pub fn score(&self) -> f64 {
        let per_week = |days: u64| 7.0 / days as f64;
        match self {
            Self::FunnelDrop {
                recent, baseline, ..
            } => {
                (rate(baseline.to, baseline.from) - rate(recent.to, recent.from))
                    * recent.from as f64
                    * per_week(FUNNEL_RECENT_DAYS)
            }
            Self::LowAddToCart {
                views,
                adds,
                shop_views,
                shop_adds,
                ..
            } => {
                (rate(*shop_adds, *shop_views) - rate(*adds, *views))
                    * *views as f64
                    * per_week(PRODUCT_DAYS)
            }
            Self::SlowPage {
                metric,
                p75,
                requests,
                ..
            } => {
                let bound = metric.good();
                *requests as f64
                    * per_week(VITALS_DAYS)
                    * ((p75 - bound) / bound).min(1.0)
                    * SLOW_PAGE_WEIGHT
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Metrics {
    pub funnel_recent: [i64; 5],
    pub funnel_baseline: [i64; 5],
    pub products: Vec<(Uuid, i64, i64)>,
    pub vitals: Vec<VitalStat>,
    pub requests: BTreeMap<String, i64>,
}

#[derive(Debug, Clone)]
pub struct VitalStat {
    pub template: String,
    pub metric: Vital,
    pub p75: f64,
    pub samples: i64,
}

fn dropped(recent: Pair, baseline: Pair) -> bool {
    let has_rate = |p: Pair| p.from >= FUNNEL_MIN_FROM && (0..=p.from).contains(&p.to);
    if !has_rate(recent) || !has_rate(baseline) {
        return false;
    }
    let (r, b) = (
        rate(recent.to, recent.from),
        rate(baseline.to, baseline.from),
    );
    let pooled = rate(recent.to + baseline.to, recent.from + baseline.from);
    let variance =
        pooled * (1.0 - pooled) * (1.0 / recent.from as f64 + 1.0 / baseline.from as f64);
    variance > 0.0 && r <= FUNNEL_MAX_RATIO * b && (b - r) / variance.sqrt() >= FUNNEL_MIN_Z
}

pub fn detect(m: &Metrics) -> Vec<Detection> {
    let mut found = Vec::new();
    for (i, transition) in Transition::ALL.into_iter().enumerate() {
        let pair = |f: &[i64; 5]| Pair {
            from: f[i],
            to: f[i + 1],
        };
        let (recent, baseline) = (pair(&m.funnel_recent), pair(&m.funnel_baseline));
        if dropped(recent, baseline) {
            found.push(Detection::FunnelDrop {
                transition,
                recent,
                baseline,
            });
        }
    }
    let shop_views: i64 = m.products.iter().map(|p| p.1).sum();
    let shop_adds: i64 = m.products.iter().map(|p| p.2).sum();
    if shop_views >= SHOP_MIN_VIEWS {
        let shop_rate = rate(shop_adds, shop_views);
        for &(product_id, views, adds) in &m.products {
            if views >= PRODUCT_MIN_VIEWS && rate(adds, views) <= PRODUCT_MAX_RATIO * shop_rate {
                found.push(Detection::LowAddToCart {
                    product_id,
                    views,
                    adds,
                    shop_views,
                    shop_adds,
                });
            }
        }
    }
    for v in &m.vitals {
        if v.samples >= VITALS_MIN_SAMPLES && v.p75 > v.metric.good() {
            found.push(Detection::SlowPage {
                template: v.template.clone(),
                metric: v.metric,
                p75: v.p75,
                samples: v.samples,
                requests: m.requests.get(&v.template).copied().unwrap_or(0),
            });
        }
    }
    found.retain(|d| d.score() > 0.0 && d.score().is_finite());
    found
}

pub fn rank(found: &mut [Detection]) {
    found.sort_by(|a, b| {
        b.score()
            .total_cmp(&a.score())
            .then_with(|| a.kind().cmp(b.kind()))
            .then_with(|| a.subject().cmp(&b.subject()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranked(m: &Metrics) -> Vec<(&'static str, String, f64)> {
        let mut found = detect(m);
        rank(&mut found);
        found
            .iter()
            .map(|d| (d.kind(), d.subject(), (d.score() * 100.0).round() / 100.0))
            .collect()
    }

    fn row(kind: &'static str, subject: &str, score: f64) -> (&'static str, String, f64) {
        (kind, subject.to_owned(), score)
    }

    fn p(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn vital(template: &str, metric: Vital, p75: f64, samples: i64) -> VitalStat {
        VitalStat {
            template: template.into(),
            metric,
            p75,
            samples,
        }
    }

    fn requests(items: &[(&str, i64)]) -> BTreeMap<String, i64> {
        items.iter().map(|(t, n)| ((*t).to_owned(), *n)).collect()
    }

    fn funnel(recent: [i64; 5], baseline: [i64; 5]) -> Metrics {
        Metrics {
            funnel_recent: recent,
            funnel_baseline: baseline,
            ..Metrics::default()
        }
    }

    fn products(items: &[(u128, i64, i64)]) -> Metrics {
        Metrics {
            products: items.iter().map(|&(id, v, a)| (p(id), v, a)).collect(),
            ..Metrics::default()
        }
    }

    #[test]
    fn a_shop_ranks_its_findings_across_kinds() {
        let m = Metrics {
            funnel_recent: [1000, 500, 60, 50, 20],
            funnel_baseline: [3000, 1500, 300, 150, 60],
            products: vec![
                (p(1), 400, 8),
                (p(2), 600, 60),
                (p(3), 150, 0),
                (p(4), 250, 30),
            ],
            vitals: vec![
                vital("product", Vital::Lcp, 4000.0, 80),
                vital("home", Vital::Inp, 260.0, 60),
                vital("category", Vital::Cls, 0.3, 100),
                vital("checkout", Vital::Lcp, 2400.0, 100),
                vital("search", Vital::Lcp, 9000.0, 49),
            ],
            requests: requests(&[("product", 20_000), ("home", 4000), ("search", 9000)]),
        };
        assert_eq!(
            ranked(&m),
            vec![
                row("slow_page", "product:LCP", 300.0),
                row("funnel_drop", "view_item_to_add_to_cart", 40.0),
                row("slow_page", "home:INP", 30.0),
                row(
                    "low_add_to_cart",
                    "00000000-0000-0000-0000-000000000001",
                    10.0
                ),
            ]
        );
    }

    #[test]
    fn equal_scores_order_by_kind_then_subject() {
        let m = Metrics {
            funnel_recent: [200, 50, 0, 0, 0],
            funnel_baseline: [2000, 1000, 0, 0, 0],
            products: vec![(p(9), 1000, 200), (p(8), 1000, 0)],
            vitals: vec![
                vital("page", Vital::Lcp, 5000.0, 50),
                vital("blog", Vital::Lcp, 5000.0, 50),
            ],
            requests: requests(&[("page", 2000), ("blog", 2000)]),
        };
        assert_eq!(
            ranked(&m),
            vec![
                row("funnel_drop", "sessions_to_view_item", 50.0),
                row(
                    "low_add_to_cart",
                    "00000000-0000-0000-0000-000000000008",
                    50.0
                ),
                row("slow_page", "blog:LCP", 50.0),
                row("slow_page", "page:LCP", 50.0),
            ]
        );
    }

    #[test]
    fn funnel_needs_100_recent_from_step_sessions() {
        let at = funnel([100, 20, 0, 0, 0], [1000, 500, 0, 0, 0]);
        assert_eq!(
            ranked(&at),
            vec![row("funnel_drop", "sessions_to_view_item", 30.0)]
        );
        assert_eq!(
            ranked(&funnel([99, 20, 0, 0, 0], [1000, 500, 0, 0, 0])),
            vec![]
        );
    }

    #[test]
    fn funnel_needs_100_baseline_from_step_sessions() {
        let at = funnel([1000, 200, 0, 0, 0], [100, 50, 0, 0, 0]);
        assert_eq!(
            ranked(&at),
            vec![row("funnel_drop", "sessions_to_view_item", 300.0)]
        );
        assert_eq!(
            ranked(&funnel([1000, 200, 0, 0, 0], [99, 50, 0, 0, 0])),
            vec![]
        );
    }

    #[test]
    fn funnel_fires_at_80_percent_of_the_baseline_rate() {
        let at = funnel([1000, 400, 0, 0, 0], [10_000, 5000, 0, 0, 0]);
        assert_eq!(
            ranked(&at),
            vec![row("funnel_drop", "sessions_to_view_item", 100.0)]
        );
        let above = funnel([1000, 401, 0, 0, 0], [10_000, 5000, 0, 0, 0]);
        assert_eq!(ranked(&above), vec![]);
    }

    #[test]
    fn funnel_drop_must_be_significant() {
        let significant = funnel([100, 38, 0, 0, 0], [300, 150, 0, 0, 0]);
        assert_eq!(
            ranked(&significant),
            vec![row("funnel_drop", "sessions_to_view_item", 12.0)]
        );
        let noise = funnel([100, 39, 0, 0, 0], [300, 150, 0, 0, 0]);
        assert_eq!(ranked(&noise), vec![]);
    }

    #[test]
    fn a_non_strict_funnel_step_yields_no_signal_and_no_nan() {
        let m = funnel([0, 400, 200, 0, 0], [0, 200, 300, 0, 0]);
        let found = detect(&m);
        assert_eq!(found, vec![]);
        let zero = funnel([0, 0, 300, 0, 0], [0, 0, 300, 0, 0]);
        assert_eq!(ranked(&zero), vec![]);
    }

    #[test]
    fn products_need_200_views() {
        let at = products(&[(1, 200, 0), (2, 800, 80)]);
        assert_eq!(
            ranked(&at),
            vec![row(
                "low_add_to_cart",
                "00000000-0000-0000-0000-000000000001",
                8.0
            )]
        );
        assert_eq!(ranked(&products(&[(1, 199, 0), (2, 801, 80)])), vec![]);
    }

    #[test]
    fn the_shop_needs_1000_views() {
        assert_eq!(ranked(&products(&[(1, 200, 0), (2, 799, 80)])), vec![]);
    }

    #[test]
    fn products_fire_at_half_the_shop_rate() {
        let at = products(&[(1, 200, 8), (2, 800, 72)]);
        assert_eq!(
            ranked(&at),
            vec![row(
                "low_add_to_cart",
                "00000000-0000-0000-0000-000000000001",
                4.0
            )]
        );
        assert_eq!(ranked(&products(&[(1, 200, 9), (2, 800, 71)])), vec![]);
    }

    #[test]
    fn a_shop_without_add_to_carts_has_no_product_signal() {
        assert_eq!(ranked(&products(&[(1, 600, 0), (2, 600, 0)])), vec![]);
    }

    fn vitals(items: Vec<VitalStat>) -> Metrics {
        Metrics {
            vitals: items,
            requests: requests(&[("home", 2000), ("product", 2000), ("category", 2000)]),
            ..Metrics::default()
        }
    }

    #[test]
    fn vitals_need_50_samples() {
        let at = vitals(vec![vital("home", Vital::Lcp, 5000.0, 50)]);
        assert_eq!(ranked(&at), vec![row("slow_page", "home:LCP", 50.0)]);
        assert_eq!(
            ranked(&vitals(vec![vital("home", Vital::Lcp, 5000.0, 49)])),
            vec![]
        );
    }

    #[test]
    fn vitals_fire_only_above_the_good_bound() {
        let at = vitals(vec![
            vital("home", Vital::Lcp, 2500.0, 100),
            vital("product", Vital::Inp, 200.0, 100),
            vital("category", Vital::Cls, 0.1, 100),
        ]);
        assert_eq!(ranked(&at), vec![]);
        let above = vitals(vec![
            vital("home", Vital::Lcp, 2600.0, 100),
            vital("product", Vital::Inp, 250.0, 100),
            vital("category", Vital::Cls, 0.15, 100),
        ]);
        assert_eq!(
            ranked(&above),
            vec![
                row("slow_page", "category:CLS", 25.0),
                row("slow_page", "product:INP", 12.5),
                row("slow_page", "home:LCP", 2.0),
            ]
        );
    }

    #[test]
    fn a_slow_template_without_requests_has_no_signal() {
        let m = Metrics {
            vitals: vec![vital("blog", Vital::Lcp, 9000.0, 500)],
            ..Metrics::default()
        };
        assert_eq!(ranked(&m), vec![]);
    }

    #[test]
    fn detections_serialize_tagged_by_kind() {
        let d = Detection::SlowPage {
            template: "home".into(),
            metric: Vital::Inp,
            p75: 260.0,
            samples: 60,
            requests: 4000,
        };
        assert_eq!(
            serde_json::to_value(&d).unwrap(),
            serde_json::json!({ "kind": "slow_page", "template": "home", "metric": "INP",
                                "p75": 260.0, "samples": 60, "requests": 4000 })
        );
        let f = Detection::FunnelDrop {
            transition: Transition::BeginCheckoutToPurchase,
            recent: Pair { from: 100, to: 10 },
            baseline: Pair { from: 300, to: 90 },
        };
        assert_eq!(
            serde_json::to_value(&f).unwrap()["transition"],
            f.subject().as_str()
        );
    }
}
