mod problems;

use crate::problems::p00;
use crate::problems::p01;
use crate::problems::p02;
use crate::problems::p03;
use crate::problems::p04;
use crate::problems::p05;
use crate::problems::p06;
use crate::problems::p07;
use crate::problems::p08;

use tracing_error::ErrorLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[tokio::main]
async fn main() {
    let fmt_layer = tracing_subscriber::fmt::layer().with_target(false);
    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .with(ErrorLayer::default())
        .init();

    color_eyre::install().expect("failed to install panic handler");

    let _ = tokio::join!(
        tokio::spawn(p00::run()),
        tokio::spawn(p01::run()),
        tokio::spawn(p02::run()),
        tokio::spawn(p03::run()),
        tokio::spawn(p04::run()),
        tokio::spawn(p05::run()),
        tokio::spawn(p06::run()),
        tokio::spawn(p07::run()),
        tokio::spawn(p08::run())
    );
}
