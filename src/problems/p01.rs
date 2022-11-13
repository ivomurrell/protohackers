use num_bigint::BigInt;
use num_traits::{One, Zero};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{de::Error, Deserialize, Deserializer, Serialize};
use serde_json::Number;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};
use tracing::{info, instrument, Instrument};

#[derive(Deserialize, Debug)]
struct Request<'a> {
    method: &'a str,
    #[serde(deserialize_with = "deserialise_big_int")]
    number: Option<BigInt>,
}

#[derive(Serialize, Debug)]
struct Response<'a> {
    method: &'a str,
    prime: bool,
}

fn deserialise_big_int<'de, D: Deserializer<'de>>(
    deserialiser: D,
) -> Result<Option<BigInt>, D::Error> {
    let n = Number::deserialize(deserialiser)?;
    let string = n.to_string();
    static DECIMAL_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"^(-?\d+)(?:\.(\d*))?$").expect("failed to compile regex"));
    let caps = DECIMAL_RE
        .captures(&string)
        .expect("stringified number had no digits");
    if let Some(decimal) = caps.get(2) {
        if decimal.as_str().parse() != Ok(0) {
            return Ok(None);
        }
    }
    let bytes = caps[1].as_bytes();
    BigInt::parse_bytes(bytes, 10)
        .map(Some)
        .ok_or_else(|| D::Error::invalid_value(serde::de::Unexpected::Bytes(bytes), &"integer"))
}

fn validate_request(raw: &str) -> Option<Request> {
    let req: Request = serde_json::from_str(raw).ok()?;
    if req.method != "isPrime" {
        return None;
    }
    Some(req)
}

fn is_prime(num: BigInt) -> bool {
    num > One::one()
        && !num_iter::range_inclusive(2.into(), num.sqrt())
            .any(|candidate| num.clone() % candidate == Zero::zero())
}

#[instrument]
pub async fn run() {
    let listener = TcpListener::bind("0.0.0.0:10001")
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
                let mut buf = String::new();

                while reader
                    .read_line(&mut buf)
                    .await
                    .expect("failed to read line from socket")
                    != 0
                {
                    info!("read request {:?}", &buf);
                    let validated = validate_request(&buf);
                    let is_valid = validated.is_some();
                    let mut resp = match validated {
                        Some(req) => {
                            let resp = Response {
                                method: "isPrime",
                                prime: match req.number {
                                    Some(num) => is_prime(num),
                                    None => false,
                                },
                            };
                            serde_json::to_string(&resp).expect("failed to serialise response")
                        }
                        None => "malformed".to_owned(),
                    };
                    resp.push('\n');
                    writer
                        .write_all(resp.as_bytes())
                        .await
                        .expect("failed to write response to socket");

                    if !is_valid {
                        return;
                    }
                    buf.clear();
                }
            }
            .instrument(tracing::info_span!("handler", ?addr)),
        );
    }
}
