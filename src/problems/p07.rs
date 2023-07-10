use nom::{
    branch::alt,
    bytes::complete::{escaped_transform, is_not, tag},
    combinator::{map_res, value},
    sequence::{delimited, preceded, terminated},
    IResult,
};
use std::{
    collections::HashMap, env, iter, net::SocketAddr, pin::Pin, str, sync::Arc, time::Duration,
};
use tokio::{
    net::UdpSocket,
    sync::{mpsc, RwLock},
    time::Sleep,
};
use tracing::{debug, error, info, instrument, warn};

type SessionToken = u32;

const MAX_DATA_SIZE: usize = 900;
const RETRANSMISSION_TIMEOUT_DURATION: Duration = Duration::from_millis(100);
const SESSION_EXPIRY_TIMEOUT_DURATION: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
enum Message {
    Connect,
    Data { pos: u32, data: String },
    Ack { length: u32 },
    Close,
}

fn parse_number(input: &str) -> IResult<&str, u32> {
    map_res(is_not("/"), str::parse)(input)
}

fn parse_message_type(input: &str) -> IResult<&str, &str> {
    alt((tag("connect"), tag("data"), tag("ack"), tag("close")))(input)
}

fn parse_session_token(input: &str) -> IResult<&str, SessionToken> {
    parse_number(input)
}

fn parse_data(input: &str) -> IResult<&str, Message> {
    let (input, pos) = terminated(parse_number, tag("/"))(input)?;
    let (input, data) = escaped_transform(
        is_not("\\/"),
        '\\',
        alt((value("/", tag("/")), value("\\", tag("\\")))),
    )(input)?;
    let (input, _) = tag("/")(input)?;
    Ok((input, Message::Data { pos, data }))
}

fn parse_ack(input: &str) -> IResult<&str, Message> {
    let (input, length) = terminated(parse_number, tag("/"))(input)?;
    Ok((input, Message::Ack { length }))
}

fn parse_message(input: &str) -> IResult<&str, (SessionToken, Message)> {
    let (input, message_type) = preceded(tag("/"), parse_message_type)(input)?;
    let (input, session_token) = delimited(tag("/"), parse_session_token, tag("/"))(input)?;
    let (input, message) = match message_type {
        "connect" => (input, Message::Connect),
        "data" => parse_data(input)?,
        "ack" => parse_ack(input)?,
        "close" => (input, Message::Close),
        _ => unreachable!(),
    };
    Ok((input, (session_token, message)))
}

fn escape_data(data: &str) -> String {
    data.replace('/', r"\/").replace('\\', r"\\")
}

fn encode_message(message: Message, session_token: SessionToken) -> String {
    match message {
        Message::Connect => format!("/connect/{session_token}/"),
        Message::Data { pos, data } => {
            format!("/data/{session_token}/{pos}/{}/", escape_data(&data))
        }
        Message::Ack { length } => format!("/ack/{session_token}/{length}/"),
        Message::Close => format!("/close/{session_token}/"),
    }
}

async fn send_message(
    message: Message,
    session_token: SessionToken,
    socket: &UdpSocket,
    client_addr: &SocketAddr,
) -> std::io::Result<()> {
    let payload = encode_message(message, session_token);
    debug!(?payload, "sending message");
    socket.send_to(payload.as_bytes(), client_addr).await?;
    Ok(())
}

struct Client {
    socket: Arc<UdpSocket>,
    client_addr: SocketAddr,
    message_listener: mpsc::Receiver<Message>,
    session_token: SessionToken,
    incoming_data_buffer: String,
    incoming_pos: u32,
    outgoing_data: String,
    max_ack: u32,
    session_timeout: Pin<Box<Sleep>>,
    retransmission_timeout: Option<Pin<Box<Sleep>>>,
}

impl Client {
    fn new(
        socket: Arc<UdpSocket>,
        client_addr: SocketAddr,
        message_listener: mpsc::Receiver<Message>,
        session_token: SessionToken,
    ) -> Self {
        Self {
            socket,
            client_addr,
            message_listener,
            session_token,
            incoming_data_buffer: String::new(),
            incoming_pos: 0,
            outgoing_data: String::new(),
            max_ack: 0,
            session_timeout: Box::pin(tokio::time::sleep(SESSION_EXPIRY_TIMEOUT_DURATION)),
            retransmission_timeout: None,
        }
    }

    #[instrument(skip(self), fields(self.client_addr = %self.client_addr, self.session_token = %self.session_token))]
    async fn handle(&mut self) {
        loop {
            tokio::select! {
                message = self
                    .message_listener
                    .recv() => {
                    self.session_timeout =
                        Box::pin(tokio::time::sleep(SESSION_EXPIRY_TIMEOUT_DURATION));
                    match message {
                        Some(Message::Connect) => {
                            send_message(
                                Message::Ack {
                                    length: 0
                                },
                                self.session_token,
                                &self.socket,
                                &self.client_addr
                            )
                                .await
                                .expect("failed to send ack to client");
                        }
                        Some(Message::Data { pos, data }) => {
                            if pos <= self.incoming_pos {
                                let new_data_idx = (self.incoming_pos - pos) as usize;
                                if new_data_idx < data.len() {
                                    self.incoming_data_buffer
                                        .push_str(&data[new_data_idx..]);
                                    self.incoming_pos +=
                                        (data.len().saturating_sub(new_data_idx)) as u32;
                                }
                            }
                            send_message(
                                Message::Ack {
                                    length: self.incoming_pos,
                                },
                                self.session_token,
                                &self.socket,
                                &self.client_addr
                            )
                                .await
                                .expect("failed to send ack to client");
                            self.run_application().await;
                        }
                        Some(Message::Ack { length }) => {
                            if length > self.max_ack {
                                let progress_made = length - self.max_ack;
                                let remaining_buffer = self.outgoing_data.len() as u32;
                                if progress_made > remaining_buffer {
                                    error!(progress_made,
                                           remaining_buffer,
                                           "client is misbehaving; closing session"
                                    );
                                    return;
                                }
                                self.outgoing_data.drain(..progress_made as usize);
                                self.max_ack = length;
                                if self.outgoing_data.is_empty() {
                                    self.retransmission_timeout = None;
                                } else {
                                    self.send_data_chunk().await;
                                }
                            }
                        }
                        Some(Message::Close) => return,
                        None => {
                            error!("server channel closed");
                            return
                        }
                    }
                }
                Some(_) = async {
                    self.retransmission_timeout.as_mut()?.await;
                    self.retransmission_timeout = None;
                    Some(())
                } => {
                    self.send_data_chunk().await;
                }
                _ = &mut self.session_timeout => {
                    warn!("session has timed out");
                    return
                }
            }
        }
    }

    async fn send_data(&mut self, data: &str) {
        self.outgoing_data.push_str(data);
        self.send_data_chunk().await;
    }

    async fn send_data_chunk(&mut self) {
        let chunk_length = MAX_DATA_SIZE.min(self.outgoing_data.len());
        let data = self.outgoing_data[..chunk_length].to_owned();
        let message = Message::Data {
            pos: self.max_ack,
            data,
        };
        info!(?message, "sending data");
        send_message(message, self.session_token, &self.socket, &self.client_addr)
            .await
            .expect("failed to send data to client");
        self.retransmission_timeout = Some(Box::pin(tokio::time::sleep(
            RETRANSMISSION_TIMEOUT_DURATION,
        )));
    }

    async fn run_application(&mut self) {
        if let Some(completed_line_idx) = self.incoming_data_buffer.rfind('\n') {
            let completed_lines: String = self
                .incoming_data_buffer
                .drain(..=completed_line_idx)
                .collect();
            for completed_line in completed_lines.lines() {
                let reversed: String = completed_line
                    .chars()
                    .rev()
                    .chain(iter::once('\n'))
                    .collect();
                self.send_data(&reversed).await;
            }
        }
    }
}

#[instrument]
pub async fn run() {
    let host = if env::var("FLY").is_ok() {
        "fly-global-services"
    } else {
        "0.0.0.0"
    };
    let listener = Arc::new(
        UdpSocket::bind((host, 10007))
            .await
            .expect("failed to bind socket"),
    );
    let mut buf = [0; 1000];
    let handlers: Arc<RwLock<HashMap<SessionToken, mpsc::Sender<Message>>>> = Default::default();

    loop {
        let (len, addr) = listener
            .recv_from(&mut buf)
            .await
            .expect("failed to receive message");
        let message = str::from_utf8(&buf[..len]).expect("message was not valid UTF-8");
        info!(?message, "received message");
        match parse_message(message) {
            Ok((remaining_input, (session_token, message))) => {
                let remaining_input = remaining_input.trim_end();
                if !remaining_input.is_empty() {
                    warn!(
                        session_token,
                        remaining_input, "message was not fully parsed"
                    );
                    continue;
                }
                let sender = handlers.read().await.get(&session_token).cloned();
                match sender {
                    Some(tx) => {
                        if let Err(err) = tx.send(message).await {
                            error!(?session_token, ?err, "failed to send message to client");
                        }
                    }
                    None => match message {
                        Message::Connect => {
                            send_message(
                                Message::Ack { length: 0 },
                                session_token,
                                &listener,
                                &addr,
                            )
                            .await
                            .expect("failed to send ack for connection");
                            let (tx, rx) = mpsc::channel(256);
                            handlers.write().await.insert(session_token, tx);
                            let handlers = Arc::clone(&handlers);
                            let listener = Arc::clone(&listener);
                            tokio::spawn(async move {
                                let mut client =
                                    Client::new(Arc::clone(&listener), addr, rx, session_token);
                                client.handle().await;
                                handlers.write().await.remove(&session_token);
                                send_message(Message::Close, session_token, &listener, &addr)
                                    .await
                                    .expect("failed to close connection");
                            });
                        }
                        _ => {
                            send_message(Message::Close, session_token, &listener, &addr)
                                .await
                                .expect("failed to close connection");
                        }
                    },
                }
            }
            Err(err) => {
                warn!(?message, ?err, "failed to parse message");
            }
        }
    }
}
