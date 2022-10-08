use std::collections::BTreeMap;

use nom::{
    branch::alt, bytes::complete::tag, number::complete::be_i32, sequence::tuple, Finish, IResult,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};
use tracing::{info, instrument, Instrument};

fn message_type(i: &[u8]) -> IResult<&[u8], &[u8]> {
    alt((tag(b"I"), tag(b"Q")))(i)
}

#[instrument]
pub async fn run() {
    let listener = TcpListener::bind("0.0.0.0:10002")
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
                let (reader, mut writer) = socket.split();
                let mut reader = BufReader::new(reader);
                let mut buf = [0; 9];
                let mut price_history = BTreeMap::new();

                while reader
                    .read_exact(&mut buf)
                    .await
                    .expect("failed to read message")
                    != 0
                {
                    info!("read request {:?}", &buf);
                    match tuple((message_type, be_i32, be_i32))(&buf)
                        .finish()
                        .expect("failed to parse message")
                        .1
                    {
                        (b"I", timestamp, price) => {
                            info!(timestamp, price, "inserting message");
                            price_history.insert(timestamp, price);
                        }
                        (b"Q", mintime, maxtime) => {
                            info!(mintime, maxtime, "querying message");
                            let average = if mintime <= maxtime {
                                let period: Vec<_> = price_history
                                    .range(mintime..=maxtime)
                                    .map(|(_, val)| *val)
                                    .collect();
                                if !period.is_empty() {
                                    (period.iter().map(|num| *num as i128).sum::<i128>()
                                        / period.len() as i128)
                                        as i32
                                } else {
                                    0
                                }
                            } else {
                                0
                            };
                            info!(average, "writing response");
                            writer
                                .write_i32(average)
                                .await
                                .expect("failed to write response");
                        }
                        _ => unreachable!(),
                    }
                }
            }
            .instrument(tracing::info_span!("handler", ?addr)),
        );
    }
}
