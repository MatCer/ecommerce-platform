//! Order, payment, cash-on-delivery, fulfillment and return-line status machines (spec §7.3,
//! A10, A13, A16).
//!
//! Pure functions: `transition(state, command)` returns the next state plus the events to
//! record (`order_events`, outbox), or a [`TransitionError`] when the command is not allowed
//! in that state. Persisting states and events is WP10-12's job; they must go through these
//! functions so that every status change is one of the transitions tested here.
//!
//! - Order: `pending → confirmed → processing → shipped → delivered`, `cancelled`, and
//!   `returned` once every line is returned. Partial returns are not an order status (A13):
//!   they are derived from the per-line return states ([`ReturnSummary`]).
//! - COD orders are `confirmed` on placement (A13); prepaid ones wait for the payment.
//! - Payment (the customer's money): `unpaid → authorized → paid → partially_refunded →
//!   refunded`, `failed`, `expired`. A payment that succeeds after the order expired or was
//!   cancelled is recorded, never refused, and raises [`PaymentEvent::LatePayment`] (A10: order
//!   exception + refund task, stock untouched).
//! - Cash on delivery (A16) is tracked separately, because the carrier's cash movements are
//!   independent of refunds to the customer: `pending → delivered → collected → remitted`.
//!   Collection is when the payment becomes `paid` ([`CodEvent::Collected`] tells the caller to
//!   apply [`PaymentCommand::Succeed`]); a later refund does not undo the carrier's remittance.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{machine}: {command} is not allowed in state {state}")]
pub struct TransitionError {
    pub machine: &'static str,
    pub state: &'static str,
    pub command: &'static str,
}

impl From<TransitionError> for platform::Error {
    fn from(e: TransitionError) -> Self {
        Self::Conflict {
            code: "invalid_transition",
            detail: e.to_string(),
        }
    }
}

/// A transition's result: the new state and what happened.
pub type Outcome<S, E> = Result<(S, Vec<E>), TransitionError>;

macro_rules! names {
    ($ty:ident { $($variant:ident => $name:literal),+ $(,)? }) => {
        impl $ty {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $name),+ }
            }
        }

        impl std::str::FromStr for $ty {
            type Err = ();

            fn from_str(s: &str) -> Result<Self, ()> {
                match s { $($name => Ok(Self::$variant),)+ _ => Err(()) }
            }
        }
    };
}

// ---------------------------------------------------------------------------------------
// Order

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Pending,
    Confirmed,
    Processing,
    Shipped,
    Delivered,
    Cancelled,
    Returned,
}

names!(OrderStatus {
    Pending => "pending",
    Confirmed => "confirmed",
    Processing => "processing",
    Shipped => "shipped",
    Delivered => "delivered",
    Cancelled => "cancelled",
    Returned => "returned",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderCommand {
    /// The payment arrived (or was authorized) for a prepaid order.
    Confirm,
    /// The merchant started packing.
    StartProcessing,
    /// The carrier took the parcel.
    Ship,
    Deliver,
    Cancel,
    /// Every line is returned (see [`ReturnSummary`]); the summary is the caller's evidence.
    MarkReturned(ReturnSummary),
}

impl OrderCommand {
    fn name(self) -> &'static str {
        match self {
            Self::Confirm => "confirm",
            Self::StartProcessing => "start_processing",
            Self::Ship => "ship",
            Self::Deliver => "deliver",
            Self::Cancel => "cancel",
            Self::MarkReturned(_) => "mark_returned",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrderEvent {
    Confirmed,
    ProcessingStarted,
    Shipped,
    Delivered,
    /// Stock reservations must be released (A13).
    Cancelled,
    Returned,
}

/// How an order is paid, as far as its lifecycle cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentKind {
    /// Card, bank transfer: the order waits for the money.
    Prepaid,
    CashOnDelivery,
}

/// The status of a newly placed order (A13: COD is confirmed on placement).
pub fn order_on_placement(kind: PaymentKind) -> (OrderStatus, Vec<OrderEvent>) {
    match kind {
        PaymentKind::Prepaid => (OrderStatus::Pending, vec![]),
        PaymentKind::CashOnDelivery => (OrderStatus::Confirmed, vec![OrderEvent::Confirmed]),
    }
}

pub fn order_transition(
    state: OrderStatus,
    command: OrderCommand,
) -> Outcome<OrderStatus, OrderEvent> {
    use OrderCommand as C;
    use OrderStatus as S;
    let next = match (state, command) {
        (S::Pending, C::Confirm) => (S::Confirmed, OrderEvent::Confirmed),
        (S::Confirmed, C::StartProcessing) => (S::Processing, OrderEvent::ProcessingStarted),
        (S::Confirmed | S::Processing, C::Ship) => (S::Shipped, OrderEvent::Shipped),
        (S::Shipped, C::Deliver) => (S::Delivered, OrderEvent::Delivered),
        // Nothing left the warehouse yet; afterwards it is a return, not a cancellation.
        (S::Pending | S::Confirmed | S::Processing, C::Cancel) => {
            (S::Cancelled, OrderEvent::Cancelled)
        }
        (S::Shipped | S::Delivered, C::MarkReturned(ReturnSummary::Full)) => {
            (S::Returned, OrderEvent::Returned)
        }
        _ => {
            return Err(TransitionError {
                machine: "order",
                state: state.as_str(),
                command: command.name(),
            });
        }
    };
    Ok((next.0, vec![next.1]))
}

// ---------------------------------------------------------------------------------------
// Payment

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    Unpaid,
    Authorized,
    Paid,
    PartiallyRefunded,
    Refunded,
    Failed,
    Expired,
}

names!(PaymentStatus {
    Unpaid => "unpaid",
    Authorized => "authorized",
    Paid => "paid",
    PartiallyRefunded => "partially_refunded",
    Refunded => "refunded",
    Failed => "failed",
    Expired => "expired",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentCommand {
    Authorize,
    /// The money is confirmed (`payment_intent.succeeded`, a matched transfer, COD collected).
    Succeed,
    Fail,
    /// The payment window ran out (payment timeouts, WP11).
    Expire,
    /// A new payment attempt after a failure (A10).
    Retry,
    Refund {
        full: bool,
    },
}

impl PaymentCommand {
    fn name(self) -> &'static str {
        match self {
            Self::Authorize => "authorize",
            Self::Succeed => "succeed",
            Self::Fail => "fail",
            Self::Expire => "expire",
            Self::Retry => "retry",
            Self::Refund { full: true } => "refund_full",
            Self::Refund { full: false } => "refund_partial",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaymentEvent {
    Authorized,
    Paid,
    Failed,
    Expired,
    RetryStarted,
    PartiallyRefunded,
    Refunded,
    /// Money arrived for an expired or cancelled order: flag the order as an exception and
    /// open a refund task; never restore stock silently (A10).
    LatePayment,
}

/// Payment transitions. `order` is the order's current status: a success on a cancelled
/// order is a late payment too.
pub fn payment_transition(
    state: PaymentStatus,
    command: PaymentCommand,
    order: OrderStatus,
) -> Outcome<PaymentStatus, PaymentEvent> {
    use PaymentCommand as C;
    use PaymentEvent as E;
    use PaymentStatus as S;
    let late = |mut events: Vec<E>| {
        if state == S::Expired || order == OrderStatus::Cancelled {
            events.push(E::LatePayment);
        }
        events
    };
    Ok(match (state, command) {
        (S::Unpaid | S::Failed, C::Authorize) => (S::Authorized, vec![E::Authorized]),
        (S::Unpaid | S::Authorized | S::Failed | S::Expired, C::Succeed) => {
            (S::Paid, late(vec![E::Paid]))
        }
        (S::Unpaid | S::Authorized, C::Fail) => (S::Failed, vec![E::Failed]),
        (S::Unpaid | S::Authorized | S::Failed, C::Expire) => (S::Expired, vec![E::Expired]),
        (S::Failed, C::Retry) => (S::Unpaid, vec![E::RetryStarted]),
        (S::Paid | S::PartiallyRefunded, C::Refund { full: true }) => {
            (S::Refunded, vec![E::Refunded])
        }
        (S::Paid | S::PartiallyRefunded, C::Refund { full: false }) => {
            (S::PartiallyRefunded, vec![E::PartiallyRefunded])
        }
        _ => {
            return Err(TransitionError {
                machine: "payment",
                state: state.as_str(),
                command: command.name(),
            });
        }
    })
}

// ---------------------------------------------------------------------------------------
// Cash on delivery (A16)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodStatus {
    /// The parcel is on its way; nobody holds the cash yet.
    Pending,
    /// Delivered; the carrier took the cash (not yet reported).
    Delivered,
    /// The carrier (or the merchant) reported the cash as collected.
    Collected,
    /// The carrier paid the cash out to the merchant (payout import or manual, audited).
    Remitted,
}

names!(CodStatus {
    Pending => "pending",
    Delivered => "delivered",
    Collected => "collected",
    Remitted => "remitted",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodCommand {
    Deliver,
    Collect,
    Remit,
}

impl CodCommand {
    fn name(self) -> &'static str {
        match self {
            Self::Deliver => "deliver",
            Self::Collect => "collect",
            Self::Remit => "remit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodEvent {
    Delivered,
    /// The customer paid: apply [`PaymentCommand::Succeed`] to the payment.
    Collected,
    Remitted,
}

pub fn cod_transition(state: CodStatus, command: CodCommand) -> Outcome<CodStatus, CodEvent> {
    use CodCommand as C;
    use CodStatus as S;
    let next = match (state, command) {
        (S::Pending, C::Deliver) => (S::Delivered, CodEvent::Delivered),
        // The carrier's report can skip the delivery step; a manual confirmation too.
        (S::Pending | S::Delivered, C::Collect) => (S::Collected, CodEvent::Collected),
        (S::Collected, C::Remit) => (S::Remitted, CodEvent::Remitted),
        _ => {
            return Err(TransitionError {
                machine: "cod",
                state: state.as_str(),
                command: command.name(),
            });
        }
    };
    Ok((next.0, vec![next.1]))
}

// ---------------------------------------------------------------------------------------
// Fulfillment

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FulfillmentStatus {
    Unfulfilled,
    LabelCreated,
    Shipped,
    Delivered,
    /// The parcel came back (refused, not picked up, returned as a whole).
    Returned,
}

names!(FulfillmentStatus {
    Unfulfilled => "unfulfilled",
    LabelCreated => "label_created",
    Shipped => "shipped",
    Delivered => "delivered",
    Returned => "returned",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FulfillmentCommand {
    CreateLabel,
    CancelLabel,
    Ship,
    Deliver,
    ReturnToSender,
}

impl FulfillmentCommand {
    fn name(self) -> &'static str {
        match self {
            Self::CreateLabel => "create_label",
            Self::CancelLabel => "cancel_label",
            Self::Ship => "ship",
            Self::Deliver => "deliver",
            Self::ReturnToSender => "return_to_sender",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FulfillmentEvent {
    LabelCreated,
    LabelCancelled,
    /// Reserved stock is committed (A13).
    Shipped,
    Delivered,
    Returned,
}

pub fn fulfillment_transition(
    state: FulfillmentStatus,
    command: FulfillmentCommand,
) -> Outcome<FulfillmentStatus, FulfillmentEvent> {
    use FulfillmentCommand as C;
    use FulfillmentEvent as E;
    use FulfillmentStatus as S;
    let next = match (state, command) {
        (S::Unfulfilled, C::CreateLabel) => (S::LabelCreated, E::LabelCreated),
        (S::LabelCreated, C::CancelLabel) => (S::Unfulfilled, E::LabelCancelled),
        (S::LabelCreated, C::Ship) => (S::Shipped, E::Shipped),
        (S::Shipped, C::Deliver) => (S::Delivered, E::Delivered),
        (S::Shipped | S::Delivered, C::ReturnToSender) => (S::Returned, E::Returned),
        _ => {
            return Err(TransitionError {
                machine: "fulfillment",
                state: state.as_str(),
                command: command.name(),
            });
        }
    };
    Ok((next.0, vec![next.1]))
}

// ---------------------------------------------------------------------------------------
// Returns (per line and quantity, A13, A19)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReturnLineStatus {
    /// Declared by the customer (withdrawal or complaint).
    Requested,
    /// Accepted by the merchant; waiting for the goods (or proof of dispatch).
    Approved,
    /// The goods arrived (merchant-confirmed): restocked (A13); the refund is still due.
    Received,
    /// Refunded on proof of dispatch (A19); the goods have not arrived yet.
    RefundedAwaitingGoods,
    /// Refunded and the goods are back: done.
    Refunded,
    Rejected,
}

names!(ReturnLineStatus {
    Requested => "requested",
    Approved => "approved",
    Received => "received",
    RefundedAwaitingGoods => "refunded_awaiting_goods",
    Refunded => "refunded",
    Rejected => "rejected",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnCommand {
    Approve,
    Reject,
    /// The goods arrived at the merchant.
    Receive,
    Refund,
}

impl ReturnCommand {
    fn name(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject => "reject",
            Self::Receive => "receive",
            Self::Refund => "refund",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReturnEvent {
    Approved,
    Rejected,
    /// Restock the returned quantity (A13). Emitted exactly once per return line.
    GoodsReceived,
    Refunded,
}

pub fn return_transition(
    state: ReturnLineStatus,
    command: ReturnCommand,
) -> Outcome<ReturnLineStatus, ReturnEvent> {
    use ReturnCommand as C;
    use ReturnEvent as E;
    use ReturnLineStatus as S;
    let next = match (state, command) {
        (S::Requested, C::Approve) => (S::Approved, E::Approved),
        (S::Requested, C::Reject) => (S::Rejected, E::Rejected),
        (S::Approved, C::Receive) => (S::Received, E::GoodsReceived),
        (S::Approved, C::Refund) => (S::RefundedAwaitingGoods, E::Refunded),
        (S::Received, C::Refund) => (S::Refunded, E::Refunded),
        (S::RefundedAwaitingGoods, C::Receive) => (S::Refunded, E::GoodsReceived),
        _ => {
            return Err(TransitionError {
                machine: "return",
                state: state.as_str(),
                command: command.name(),
            });
        }
    };
    Ok((next.0, vec![next.1]))
}

/// How much of an order came back, derived from its lines (A13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReturnSummary {
    None,
    Partial,
    Full,
}

/// One order line and its return lines: `(ordered quantity, [(returned quantity, state)])`.
/// A quantity counts as returned once it is received or refunded.
pub fn return_summary(lines: &[(u32, Vec<(u32, ReturnLineStatus)>)]) -> ReturnSummary {
    let mut ordered = 0u64;
    let mut returned = 0u64;
    for (quantity, returns) in lines {
        let back: u64 = returns
            .iter()
            .filter(|(_, s)| {
                matches!(
                    s,
                    ReturnLineStatus::Received
                        | ReturnLineStatus::RefundedAwaitingGoods
                        | ReturnLineStatus::Refunded
                )
            })
            .map(|(q, _)| u64::from(*q))
            .sum();
        ordered += u64::from(*quantity);
        returned += back.min(u64::from(*quantity));
    }
    match returned {
        0 => ReturnSummary::None,
        r if r >= ordered => ReturnSummary::Full,
        _ => ReturnSummary::Partial,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const ORDER_COMMANDS: &[OrderCommand] = &[
        OrderCommand::Confirm,
        OrderCommand::StartProcessing,
        OrderCommand::Ship,
        OrderCommand::Deliver,
        OrderCommand::Cancel,
        OrderCommand::MarkReturned(ReturnSummary::None),
        OrderCommand::MarkReturned(ReturnSummary::Partial),
        OrderCommand::MarkReturned(ReturnSummary::Full),
    ];

    /// Every (state, command) pair: exactly the listed ones succeed, everything else is refused.
    #[test]
    fn order_machine_is_exactly_the_spec() {
        let allowed: BTreeSet<(&str, &str, &str)> = [
            ("pending", "confirm", "confirmed"),
            ("confirmed", "start_processing", "processing"),
            ("confirmed", "ship", "shipped"),
            ("processing", "ship", "shipped"),
            ("shipped", "deliver", "delivered"),
            ("pending", "cancel", "cancelled"),
            ("confirmed", "cancel", "cancelled"),
            ("processing", "cancel", "cancelled"),
            ("shipped", "mark_returned", "returned"),
            ("delivered", "mark_returned", "returned"),
        ]
        .into();
        let mut seen = BTreeSet::new();
        for &state in OrderStatus::ALL {
            for &command in ORDER_COMMANDS {
                match order_transition(state, command) {
                    Ok((next, events)) => {
                        assert_eq!(events.len(), 1);
                        seen.insert((state.as_str(), command.name(), next.as_str()));
                    }
                    Err(e) => assert_eq!((e.machine, e.state), ("order", state.as_str())),
                }
            }
        }
        assert_eq!(seen, allowed);
        // Only a full return makes the order `returned` (A13: no partially_returned status).
        assert!(
            order_transition(
                OrderStatus::Delivered,
                OrderCommand::MarkReturned(ReturnSummary::Partial)
            )
            .is_err()
        );
    }

    #[test]
    fn cod_is_confirmed_on_placement_prepaid_waits() {
        assert_eq!(
            order_on_placement(PaymentKind::CashOnDelivery),
            (OrderStatus::Confirmed, vec![OrderEvent::Confirmed])
        );
        assert_eq!(
            order_on_placement(PaymentKind::Prepaid),
            (OrderStatus::Pending, vec![])
        );
    }

    #[test]
    fn terminal_order_states_refuse_everything() {
        for state in [OrderStatus::Cancelled, OrderStatus::Returned] {
            for &command in ORDER_COMMANDS {
                assert!(
                    order_transition(state, command).is_err(),
                    "{state:?} {command:?}"
                );
            }
        }
    }

    const PAYMENT_COMMANDS: &[PaymentCommand] = &[
        PaymentCommand::Authorize,
        PaymentCommand::Succeed,
        PaymentCommand::Fail,
        PaymentCommand::Expire,
        PaymentCommand::Retry,
        PaymentCommand::Refund { full: true },
        PaymentCommand::Refund { full: false },
    ];

    #[test]
    fn payment_machine_is_exactly_the_spec() {
        let allowed: BTreeSet<(&str, &str, &str)> = [
            ("unpaid", "authorize", "authorized"),
            ("failed", "authorize", "authorized"),
            ("unpaid", "succeed", "paid"),
            ("authorized", "succeed", "paid"),
            ("failed", "succeed", "paid"),
            ("expired", "succeed", "paid"),
            ("unpaid", "fail", "failed"),
            ("authorized", "fail", "failed"),
            ("unpaid", "expire", "expired"),
            ("authorized", "expire", "expired"),
            ("failed", "expire", "expired"),
            ("failed", "retry", "unpaid"),
            ("paid", "refund_full", "refunded"),
            ("paid", "refund_partial", "partially_refunded"),
            ("partially_refunded", "refund_full", "refunded"),
            ("partially_refunded", "refund_partial", "partially_refunded"),
        ]
        .into();
        let mut seen = BTreeSet::new();
        for &state in PaymentStatus::ALL {
            for &command in PAYMENT_COMMANDS {
                if let Ok((next, _)) = payment_transition(state, command, OrderStatus::Confirmed) {
                    seen.insert((state.as_str(), command.name(), next.as_str()));
                }
            }
        }
        assert_eq!(seen, allowed);
    }

    #[test]
    fn paid_never_regresses() {
        for command in [
            PaymentCommand::Fail,
            PaymentCommand::Expire,
            PaymentCommand::Authorize,
            PaymentCommand::Succeed,
        ] {
            assert!(
                payment_transition(PaymentStatus::Paid, command, OrderStatus::Confirmed).is_err()
            );
        }
    }

    #[test]
    fn late_payments_are_recorded_and_flagged() {
        let (s, e) = payment_transition(
            PaymentStatus::Expired,
            PaymentCommand::Succeed,
            OrderStatus::Pending,
        )
        .unwrap();
        assert_eq!(s, PaymentStatus::Paid);
        assert_eq!(e, vec![PaymentEvent::Paid, PaymentEvent::LatePayment]);
        let (_, e) = payment_transition(
            PaymentStatus::Unpaid,
            PaymentCommand::Succeed,
            OrderStatus::Cancelled,
        )
        .unwrap();
        assert_eq!(e, vec![PaymentEvent::Paid, PaymentEvent::LatePayment]);
        let (_, e) = payment_transition(
            PaymentStatus::Authorized,
            PaymentCommand::Succeed,
            OrderStatus::Pending,
        )
        .unwrap();
        assert_eq!(e, vec![PaymentEvent::Paid]);
    }

    #[test]
    fn cod_machine_is_exactly_the_spec() {
        let commands = [CodCommand::Deliver, CodCommand::Collect, CodCommand::Remit];
        let allowed: BTreeSet<(&str, &str, &str)> = [
            ("pending", "deliver", "delivered"),
            ("pending", "collect", "collected"),
            ("delivered", "collect", "collected"),
            ("collected", "remit", "remitted"),
        ]
        .into();
        let mut seen = BTreeSet::new();
        for &state in CodStatus::ALL {
            for command in commands {
                if let Ok((next, _)) = cod_transition(state, command) {
                    seen.insert((state.as_str(), command.name(), next.as_str()));
                }
            }
        }
        assert_eq!(seen, allowed);
    }

    /// A16 + refunds: collection pays the order, a refund to the customer does not stop the
    /// carrier's payout from being recorded.
    #[test]
    fn cod_collection_refund_then_remittance() {
        let (cod, events) = cod_transition(CodStatus::Delivered, CodCommand::Collect).unwrap();
        assert_eq!(events, vec![CodEvent::Collected]);
        let (payment, _) = payment_transition(
            PaymentStatus::Unpaid,
            PaymentCommand::Succeed,
            OrderStatus::Delivered,
        )
        .unwrap();
        let (payment, _) = payment_transition(
            payment,
            PaymentCommand::Refund { full: true },
            OrderStatus::Returned,
        )
        .unwrap();
        assert_eq!(payment, PaymentStatus::Refunded);
        assert_eq!(
            cod_transition(cod, CodCommand::Remit).unwrap().0,
            CodStatus::Remitted
        );
        assert!(
            cod_transition(CodStatus::Delivered, CodCommand::Remit).is_err(),
            "cash must be collected before it can be remitted"
        );
    }

    #[test]
    fn fulfillment_machine_is_exactly_the_spec() {
        let commands = [
            FulfillmentCommand::CreateLabel,
            FulfillmentCommand::CancelLabel,
            FulfillmentCommand::Ship,
            FulfillmentCommand::Deliver,
            FulfillmentCommand::ReturnToSender,
        ];
        let allowed: BTreeSet<(&str, &str, &str)> = [
            ("unfulfilled", "create_label", "label_created"),
            ("label_created", "cancel_label", "unfulfilled"),
            ("label_created", "ship", "shipped"),
            ("shipped", "deliver", "delivered"),
            ("shipped", "return_to_sender", "returned"),
            ("delivered", "return_to_sender", "returned"),
        ]
        .into();
        let mut seen = BTreeSet::new();
        for &state in FulfillmentStatus::ALL {
            for command in commands {
                if let Ok((next, events)) = fulfillment_transition(state, command) {
                    assert_eq!(events.len(), 1);
                    seen.insert((state.as_str(), command.name(), next.as_str()));
                }
            }
        }
        assert_eq!(seen, allowed);
    }

    const RETURN_COMMANDS: [ReturnCommand; 4] = [
        ReturnCommand::Approve,
        ReturnCommand::Reject,
        ReturnCommand::Receive,
        ReturnCommand::Refund,
    ];

    #[test]
    fn return_machine_is_exactly_the_spec() {
        let allowed: BTreeSet<(&str, &str, &str)> = [
            ("requested", "approve", "approved"),
            ("requested", "reject", "rejected"),
            ("approved", "receive", "received"),
            ("approved", "refund", "refunded_awaiting_goods"),
            ("received", "refund", "refunded"),
            ("refunded_awaiting_goods", "receive", "refunded"),
        ]
        .into();
        let mut seen = BTreeSet::new();
        for &state in ReturnLineStatus::ALL {
            for command in RETURN_COMMANDS {
                if let Ok((next, _)) = return_transition(state, command) {
                    seen.insert((state.as_str(), command.name(), next.as_str()));
                }
            }
        }
        assert_eq!(seen, allowed);
    }

    /// Both orders of receipt and refund restock exactly once and end `refunded`.
    #[test]
    fn goods_are_restocked_exactly_once_in_either_order() {
        for order in [
            [ReturnCommand::Receive, ReturnCommand::Refund],
            [ReturnCommand::Refund, ReturnCommand::Receive],
        ] {
            let mut state = ReturnLineStatus::Approved;
            let mut restocks = 0;
            for command in order {
                let (next, events) = return_transition(state, command).unwrap();
                restocks += events
                    .iter()
                    .filter(|e| **e == ReturnEvent::GoodsReceived)
                    .count();
                state = next;
            }
            assert_eq!(state, ReturnLineStatus::Refunded);
            assert_eq!(restocks, 1, "{order:?}");
            for command in RETURN_COMMANDS {
                assert!(return_transition(state, command).is_err(), "{command:?}");
            }
        }
    }

    #[test]
    fn return_summary_counts_received_or_refunded_quantities() {
        use ReturnLineStatus as R;
        assert_eq!(return_summary(&[(2, vec![])]), ReturnSummary::None);
        assert_eq!(
            return_summary(&[(2, vec![(1, R::Requested), (1, R::Approved)])]),
            ReturnSummary::None
        );
        assert_eq!(
            return_summary(&[(2, vec![(1, R::Received)]), (1, vec![])]),
            ReturnSummary::Partial
        );
        assert_eq!(
            return_summary(&[
                (2, vec![(1, R::Received), (1, R::RefundedAwaitingGoods)]),
                (1, vec![(1, R::Refunded)])
            ]),
            ReturnSummary::Full
        );
        assert_eq!(
            return_summary(&[(1, vec![(1, R::Rejected)])]),
            ReturnSummary::None
        );
        // Over-reported quantities never make another line count as returned.
        assert_eq!(
            return_summary(&[(1, vec![(5, R::Received)]), (1, vec![])]),
            ReturnSummary::Partial
        );
    }

    #[test]
    fn names_round_trip() {
        for s in OrderStatus::ALL {
            assert_eq!(s.as_str().parse::<OrderStatus>(), Ok(*s));
        }
        for s in PaymentStatus::ALL {
            assert_eq!(s.as_str().parse::<PaymentStatus>(), Ok(*s));
        }
        for s in CodStatus::ALL {
            assert_eq!(s.as_str().parse::<CodStatus>(), Ok(*s));
        }
        for s in FulfillmentStatus::ALL {
            assert_eq!(s.as_str().parse::<FulfillmentStatus>(), Ok(*s));
        }
        for s in ReturnLineStatus::ALL {
            assert_eq!(s.as_str().parse::<ReturnLineStatus>(), Ok(*s));
        }
    }
}
