use std::iter;

use nom::{
    branch::alt,
    bytes::complete::tag,
    combinator::{map, value},
    multi::many0,
    number::complete::u8,
    sequence::{preceded, terminated},
    IResult,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{
        tcp::{ReadHalf, WriteHalf},
        TcpListener,
    },
};
use tracing::{info, instrument, warn, Instrument};

#[derive(Debug, Clone)]
enum CipherOp {
    Rev,
    Xor(u8),
    XorPos,
    Add(u8),
    AddPos,
}
type CipherSpec = Vec<CipherOp>;

fn parse_cipher_op(input: &[u8]) -> IResult<&[u8], CipherOp> {
    alt((
        value(CipherOp::Rev, tag([0x1])),
        preceded(tag([0x2]), map(u8, CipherOp::Xor)),
        value(CipherOp::XorPos, tag([0x3])),
        preceded(tag([0x4]), map(u8, CipherOp::Add)),
        value(CipherOp::AddPos, tag([0x5])),
    ))(input)
}

fn parse_cipher_spec(input: &[u8]) -> IResult<&[u8], CipherSpec> {
    terminated(many0(parse_cipher_op), tag([0x0]))(input)
}

fn toy_priority(request: &str) -> &str {
    request
        .split(',')
        .max_by_key(|toy_request| {
            toy_request[..toy_request.find('x').expect("failed to find toy count")]
                .parse::<u32>()
                .expect("failed to parse toy count")
        })
        .expect("no toys in request")
}

enum CipherDirection {
    Encrypt,
    Decrypt,
}

struct Encoder<'a> {
    spec: &'a CipherSpec,
    incoming_pos: u8,
    outgoing_pos: u8,
}

impl<'a> Encoder<'a> {
    fn new(spec: &'a CipherSpec) -> Self {
        Self {
            spec,
            incoming_pos: 0,
            outgoing_pos: 0,
        }
    }

    fn apply_cipher(&mut self, mut byte: u8, direction: CipherDirection) -> u8 {
        let pos = match direction {
            CipherDirection::Encrypt => &mut self.outgoing_pos,
            CipherDirection::Decrypt => &mut self.incoming_pos,
        };
        let mut spec: Box<dyn DoubleEndedIterator<Item = _>> = Box::new(self.spec.iter());
        if let CipherDirection::Decrypt = direction {
            spec = Box::new(spec.rev());
        }
        for op in spec {
            let mut reversible_add = |n: u8| match direction {
                CipherDirection::Encrypt => byte = byte.wrapping_add(n),
                CipherDirection::Decrypt => byte = byte.wrapping_sub(n),
            };
            match op {
                CipherOp::Rev => byte = byte.reverse_bits(),
                CipherOp::Xor(n) => byte ^= n,
                CipherOp::XorPos => byte ^= *pos,
                CipherOp::Add(n) => reversible_add(*n),
                CipherOp::AddPos => reversible_add(*pos),
            }
        }
        *pos = pos.wrapping_add(1);
        byte
    }
}

fn is_noop_cipher(spec: &CipherSpec) -> bool {
    let mut encoder = Encoder::new(spec);
    let test_message = "10x toy car,15x dog on a string,4x inflatable motorcycle";
    test_message
        .as_bytes()
        .iter()
        .all(|byte| encoder.apply_cipher(*byte, CipherDirection::Encrypt) == *byte)
}

struct Handler<'a, 'b> {
    reader: BufReader<ReadHalf<'a>>,
    writer: WriteHalf<'a>,
    encoder: Encoder<'b>,
    decrypted_buffer: String,
}

impl<'a, 'b> Handler<'a, 'b> {
    fn new(reader: BufReader<ReadHalf<'a>>, writer: WriteHalf<'a>, spec: &'b CipherSpec) -> Self {
        Self {
            reader,
            writer,
            encoder: Encoder::new(spec),
            decrypted_buffer: String::new(),
        }
    }

    async fn handle(&mut self) {
        let mut buf = [0u8; 1024];
        loop {
            let n = self
                .reader
                .read(&mut buf)
                .await
                .expect("failed to read from socket");
            if n == 0 {
                return;
            }
            let new_data = &mut buf[..n];
            for byte in new_data.iter() {
                self.decrypted_buffer.push(
                    self.encoder
                        .apply_cipher(*byte, CipherDirection::Decrypt)
                        .into(),
                );
            }
            if let Some(last_request_idx) = self.decrypted_buffer.rfind('\n') {
                let requests: String = self.decrypted_buffer.drain(..=last_request_idx).collect();
                for request in requests.lines() {
                    let toy = toy_priority(request);
                    let encrypted: Vec<_> = toy
                        .as_bytes()
                        .iter()
                        .chain(iter::once(&b'\n'))
                        .map(|byte| self.encoder.apply_cipher(*byte, CipherDirection::Encrypt))
                        .collect();
                    self.writer
                        .write_all(&encrypted)
                        .await
                        .expect("failed to write to client");
                }
            }
        }
    }
}

#[instrument]
pub async fn run() {
    let listener = TcpListener::bind("0.0.0.0:10008")
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
                let (reader, writer) = socket.split();
                let mut reader = BufReader::new(reader);
                let mut buf = Vec::new();
                reader
                    .read_until(0x0, &mut buf)
                    .await
                    .expect("failed to read cipher spec");
                let (_, spec) = parse_cipher_spec(&buf).expect("failed to parse cipher spec");
                if is_noop_cipher(&spec) {
                    warn!(?spec, "immediately disconnecting from no-op cipher");
                    return;
                }
                Handler::new(reader, writer, &spec).handle().await;
            }
            .instrument(tracing::info_span!("handler", ?addr)),
        );
    }
}
