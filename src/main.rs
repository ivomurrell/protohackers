mod problems;

use crate::problems::p00;
use crate::problems::p01;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let _ = tokio::join!(tokio::spawn(p00::run()), tokio::spawn(p01::run()));
}
