//! `Mailer::send` outcome classification against a scripted SMTP server (spec A14).
#![allow(clippy::unwrap_used)]

use platform::mail::{Attachment, Delivery, MailConfig, Mailer, Outgoing, Stream};
use testkit::smtp::{FakeSmtp, Mode};

fn mailer(url: &str) -> Mailer {
    let cfg = MailConfig::from_lookup(&|k| {
        Some(
            match k {
                "MAIL_TRANSACTIONAL_SMTP_URL" | "MAIL_MARKETING_SMTP_URL" => url,
                "MAIL_TRANSACTIONAL_FROM" => "shop@mail.test",
                "MAIL_MARKETING_FROM" => "news@mail.test",
                _ => return None,
            }
            .to_owned(),
        )
    })
    .unwrap();
    Mailer::new(&cfg).unwrap()
}

fn message(to: &str) -> Outgoing<'_> {
    Outgoing {
        stream: Stream::Transactional,
        from_name: "Demo Shop",
        to,
        subject: "Přihlášení",
        html: "<p>Ahoj</p>",
        text: "Ahoj",
        id: "0192-test",
        list_unsubscribe: None,
        attachments: &[],
    }
}

#[tokio::test]
async fn accepted_only_after_250() {
    let smtp = FakeSmtp::start(Mode::Accept).await;
    let m = mailer(&smtp.url());
    assert_eq!(
        m.send(&message("jana@example.test")).await,
        Delivery::Accepted
    );
    let raw = smtp.received();
    assert_eq!(raw.len(), 1);
    assert!(
        raw[0].contains("Message-ID: <0192-test@mail.test>"),
        "{}",
        raw[0]
    );
    assert!(
        raw[0].contains("From: \"Demo Shop\" <shop@mail.test>"),
        "{}",
        raw[0]
    );
    assert!(raw[0].contains("multipart/alternative"));
}

#[tokio::test]
async fn attachments_make_a_mixed_message() {
    let smtp = FakeSmtp::start(Mode::Accept).await;
    let files = [Attachment {
        filename: "FV202600001.pdf".into(),
        content_type: "application/pdf".into(),
        body: b"%PDF-1.4 test".to_vec(),
    }];
    let msg = Outgoing {
        attachments: &files,
        ..message("jana@example.test")
    };
    assert_eq!(mailer(&smtp.url()).send(&msg).await, Delivery::Accepted);
    let raw = smtp.received().remove(0);
    assert!(raw.contains("multipart/mixed"), "{raw}");
    assert!(raw.contains("multipart/alternative"), "{raw}");
    assert!(raw.contains("Content-Type: application/pdf"), "{raw}");
    assert!(raw.contains("filename=\"FV202600001.pdf\""), "{raw}");
}

#[tokio::test]
async fn permanent_rejection_temporary_deferral_and_unreachable_server() {
    let smtp = FakeSmtp::start(Mode::Reject).await;
    let m = mailer(&smtp.url());
    assert!(matches!(
        m.send(&message("x@example.test")).await,
        Delivery::Rejected(_)
    ));
    smtp.set_mode(Mode::Defer);
    assert!(matches!(
        m.send(&message("x@example.test")).await,
        Delivery::NotSent(_)
    ));
    // Nothing listens on port 1: the message was never handed over.
    let down = mailer("smtp://127.0.0.1:1");
    assert!(matches!(
        down.send(&message("x@example.test")).await,
        Delivery::NotSent(_)
    ));
    // An unusable address is a permanent failure without any SMTP traffic.
    assert!(matches!(
        m.send(&message("not an address")).await,
        Delivery::Rejected(_)
    ));
}

#[tokio::test]
async fn connection_lost_after_data_is_uncertain() {
    let smtp = FakeSmtp::start(Mode::DropAfterData).await;
    let m = mailer(&smtp.url());
    assert!(matches!(
        m.send(&message("x@example.test")).await,
        Delivery::Uncertain(_)
    ));
}
