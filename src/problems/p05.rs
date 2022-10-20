use std::borrow::Cow;

use lazy_static::lazy_static;
use regex::{Captures, Regex};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};
use tracing::{debug, info, instrument, Instrument};

fn rewrite_addresses(message: &str) -> Cow<'_, str> {
    lazy_static! {
        static ref RE: Regex = Regex::new(r"\b7\w{25,34}\b").expect("failed to compile regex");
    }
    RE.replace_all(message, |caps: &Captures| {
        let address = caps.get(0).expect("no string was captured");
        let start = address.start();
        let end = message
            .chars()
            .nth(address.end())
            .expect("message ended early");
        let replacement = if (start == 0
            || message.chars().nth(start - 1).expect("message ended early") == ' ')
            && (end == '\n' || end == ' ')
        {
            "7YWHMfk9JZe0LM0g1ZauHuiSxhI"
        } else {
            address.as_str()
        };
        replacement.to_owned()
    })
}

#[instrument]
pub async fn run() {
    let listener = TcpListener::bind("0.0.0.0:10005")
        .await
        .expect("failed to bind socket");
    loop {
        let (mut socket, addr) = listener
            .accept()
            .await
            .expect("failed to accept connection");
        tokio::spawn(
            async move {
                info!("accepted connection");
                let (client_reader, mut client_writer) = socket.split();
                let mut client_reader = BufReader::new(client_reader);
                let mut client_buf = Vec::new();

                let mut upstream = TcpStream::connect("chat.protohackers.com:16963")
                    .await
                    .expect("failed to connect to upstream");
                let (upstream_reader, mut upstream_writer) = upstream.split();
                let mut upstream_reader = BufReader::new(upstream_reader);
                let mut upstream_buf = Vec::new();

                loop {
                    tokio::select! {
                        incoming = client_reader.read_until(b'\n', &mut client_buf) => {
                            let read = incoming.expect("failed to read message from client");
                            if read == 0 {
                                return;
                            }
                            let incoming = String::from_utf8(client_buf.clone())
                                .expect("client message was not UTF-8");
                            info!(incoming, "received message from client");
                            let rewritten = rewrite_addresses(&incoming);
                            debug!(%rewritten, "rewritten message from client");
                            upstream_writer
                                .write_all(rewritten.as_bytes())
                                .await
                                .expect("failed to write to upstream");
                            client_buf.clear();
                        }
                        outgoing = upstream_reader.read_until(b'\n', &mut upstream_buf) => {
                            let read = outgoing.expect("failed to read message from upstream");
                            if read == 0 {
                                return;
                            }
                            let outgoing = String::from_utf8(upstream_buf.clone())
                                .expect("upstream message was not UTF-8");
                            info!(outgoing, "received message from upstream");
                            let rewritten = rewrite_addresses(&outgoing);
                            debug!(%rewritten, "rewritten message from upstream");
                            client_writer
                                .write_all(rewritten.as_bytes())
                                .await
                                .expect("failed to write to client");
                            upstream_buf.clear();
                        }
                    }
                }
            }
            .instrument(tracing::info_span!("handler", ?addr)),
        );
    }
}
