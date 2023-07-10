mod problems;

use crate::problems::p00;
use crate::problems::p01;
use crate::problems::p02;
use crate::problems::p03;
use crate::problems::p04;
use crate::problems::p05;
use crate::problems::p06;
use crate::problems::p07;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let _ = tokio::join!(
        tokio::spawn(p00::run()),
        tokio::spawn(p01::run()),
        tokio::spawn(p02::run()),
        tokio::spawn(p03::run()),
        tokio::spawn(p04::run()),
        tokio::spawn(p05::run()),
        tokio::spawn(p06::run()),
        tokio::spawn(p07::run())
    );
}
