//! Signed SNS messages for the SES webhook tests: a throwaway RSA key and its self-signed
//! certificate (`fixtures/sns/`) stand in for an AWS SNS signing certificate.

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use base64::Engine;
use serde_json::Value;

pub const TOPIC: &str = "arn:aws:sns:eu-central-1:123456789012:ses-events";
pub const CERT_URL: &str =
    "https://sns.eu-central-1.amazonaws.com/SimpleNotificationService-0123456789abcdef.pem";
pub const CERT_PEM: &[u8] = include_bytes!("../../../fixtures/sns/test-signing-cert.pem");
const KEY_PKCS8: &[u8] = include_bytes!("../../../fixtures/sns/test-signing.pk8.der");

/// Signs `message` the way SNS does (SignatureVersion 2, RSA-SHA256 over the AWS-documented
/// `Name\nValue\n` string of the message type) with [`CERT_URL`] as `SigningCertURL`.
pub fn sign(mut message: Value) -> Value {
    let field = |name: &str| message.get(name).and_then(Value::as_str);
    let names: &[&str] = match field("Type") {
        Some("Notification") => &[
            "Message",
            "MessageId",
            "Subject",
            "Timestamp",
            "TopicArn",
            "Type",
        ],
        _ => &[
            "Message",
            "MessageId",
            "SubscribeURL",
            "Timestamp",
            "Token",
            "TopicArn",
            "Type",
        ],
    };
    let mut to_sign = String::new();
    for name in names {
        if let Some(value) = field(name) {
            to_sign += &format!("{name}\n{value}\n");
        }
    }
    let key = RsaKeyPair::from_pkcs8(KEY_PKCS8).unwrap();
    let mut signature = vec![0; key.public_modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        to_sign.as_bytes(),
        &mut signature,
    )
    .unwrap();
    message["SignatureVersion"] = "2".into();
    message["SigningCertURL"] = CERT_URL.into();
    message["Signature"] = base64::engine::general_purpose::STANDARD
        .encode(signature)
        .into();
    message
}
