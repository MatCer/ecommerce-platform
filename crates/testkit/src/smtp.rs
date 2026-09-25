//! A scripted in-process SMTP server for mail tests: accepts, rejects, defers or drops the
//! connection after the message data (the "died mid-send" case of spec A14).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 250 for everything; the message is recorded.
    Accept,
    /// 550 on RCPT TO.
    Reject,
    /// 451 on MAIL FROM.
    Defer,
    /// Reads the whole message, then closes the connection without answering.
    DropAfterData,
}

#[derive(Clone)]
pub struct FakeSmtp {
    pub addr: SocketAddr,
    mode: Arc<Mutex<Mode>>,
    received: Arc<Mutex<Vec<String>>>,
}

impl FakeSmtp {
    pub async fn start(mode: Mode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = Self {
            addr: listener.local_addr().unwrap(),
            mode: Arc::new(Mutex::new(mode)),
            received: Arc::default(),
        };
        let s = server.clone();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let s = s.clone();
                tokio::spawn(async move {
                    let _ = s.session(socket).await;
                });
            }
        });
        server
    }

    /// `smtp://127.0.0.1:<port>` for `MailConfig`.
    pub fn url(&self) -> String {
        format!("smtp://{}", self.addr)
    }

    /// A mailer whose both streams point at this server.
    pub fn mailer(&self) -> platform::mail::Mailer {
        mailer_for(&self.url())
    }

    pub fn set_mode(&self, mode: Mode) {
        *self.mode.lock().unwrap() = mode;
    }

    /// Raw messages (headers + body) accepted so far.
    pub fn received(&self) -> Vec<String> {
        self.received.lock().unwrap().clone()
    }

    async fn session(&self, socket: tokio::net::TcpStream) -> std::io::Result<()> {
        let (read, mut write) = socket.into_split();
        let mut lines = BufReader::new(read);
        write.write_all(b"220 fake ESMTP\r\n").await?;
        let mut line = String::new();
        loop {
            line.clear();
            if lines.read_line(&mut line).await? == 0 {
                return Ok(());
            }
            let mode = *self.mode.lock().unwrap();
            let cmd = line.trim_end().to_ascii_uppercase();
            let reply: &[u8] = if cmd.starts_with("EHLO") || cmd.starts_with("HELO") {
                b"250-fake\r\n250-8BITMIME\r\n250 SMTPUTF8\r\n"
            } else if cmd.starts_with("MAIL FROM") {
                if mode == Mode::Defer {
                    b"451 try again later\r\n"
                } else {
                    b"250 ok\r\n"
                }
            } else if cmd.starts_with("RCPT TO") {
                if mode == Mode::Reject {
                    b"550 no such user\r\n"
                } else {
                    b"250 ok\r\n"
                }
            } else if cmd == "DATA" {
                write.write_all(b"354 go ahead\r\n").await?;
                let mut data = String::new();
                loop {
                    line.clear();
                    if lines.read_line(&mut line).await? == 0 {
                        return Ok(());
                    }
                    if line == ".\r\n" {
                        break;
                    }
                    data.push_str(&line);
                }
                if mode == Mode::DropAfterData {
                    return Ok(());
                }
                self.received.lock().unwrap().push(data);
                b"250 queued\r\n"
            } else if cmd == "QUIT" {
                write.write_all(b"221 bye\r\n").await?;
                return Ok(());
            } else {
                b"250 ok\r\n"
            };
            write.write_all(reply).await?;
        }
    }
}

/// A mailer for `url` (both streams), e.g. `smtp://127.0.0.1:1` for an unreachable server.
pub fn mailer_for(url: &str) -> platform::mail::Mailer {
    let cfg = platform::mail::MailConfig::from_lookup(&|k| {
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
    platform::mail::Mailer::new(&cfg).unwrap()
}
