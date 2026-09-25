//! Email marketing (spec §7.6, §11.5, A12, A14, A20).
//!
//! - [`subscribers`]: double opt-in with consent evidence, unsubscribe, customer linking.
//! - [`segments`]: allowlisted rules compiled to parameterized SQL.
//! - [`campaigns`]: block content, rendering (with per-recipient personalized products),
//!   preview/test sends, scheduling, throttled idempotent batch sending, click tracking and the
//!   recipient's token actions (unsubscribe, preferences).
//! - [`deliverability`]: bounce/complaint notifications (SES via SNS) → suppression.
//!
//! Marketing mail uses the `marketing` stream of the mail pipeline: it is never resent after an
//! uncertain SMTP outcome (A14), and the recipient's status, consent and suppression are checked
//! again right before SMTP (A20, `notifications::begin_send`).

pub mod campaigns;
pub mod deliverability;
pub mod segments;
pub mod subscribers;
