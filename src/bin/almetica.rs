#![warn(clippy::all)]

use std::path::PathBuf;
use std::process;
use std::sync::Arc;

use clap::Clap;
use tokio::net::TcpListener;
use tokio::sync::mpsc::Sender;
use tokio::task;
use tracing::{error, info, info_span, warn};
use tracing_futures::Instrument;
use tracing_log::LogTracer;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::fmt::Layer;
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::Registry;

use almetica::config::read_configuration;
use almetica::dataloader::load_opcode_mapping;
use almetica::ecs::event::Event;
use almetica::ecs::world::Multiverse;
use almetica::protocol::opcode::Opcode;
use almetica::protocol::GameSession;
use almetica::web;
use almetica::Result;

#[derive(Clap)]
#[clap(version = "0.0.1", author = "Almetica <almetica@protonmail.com>")]
struct Opts {
    #[clap(short = "c", long = "config", default_value = "config.yaml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() {
    init_logging();

    if let Err(e) = run().await {
        error!("Error while executing program: {:?}", e);
        process::exit(1);
    }
}

fn init_logging() {
    let fmt_layer = Layer::builder().with_target(false).finish();
    let filter_layer = EnvFilter::from_default_env().add_directive("legion_systems::system=warn".parse().unwrap());
    let subscriber = Registry::default().with(filter_layer).with(fmt_layer);
    tracing::subscriber::set_global_default(subscriber).unwrap();
    LogTracer::init().unwrap();
}

async fn run() -> Result<()> {
    let opts: Opts = Opts::parse();

    info!("Reading configuration file");
    let config = match read_configuration(&opts.config) {
        Ok(c) => c,
        Err(e) => {
            error!("Can't read configuration file {}: {:?}", &opts.config.display(), e);
            return Err(e);
        }
    };

    info!("Reading opcode mapping file");
    let (opcode_mapping, reverse_opcode_mapping) = match load_opcode_mapping(&config.data.path) {
        Ok((opcode_mapping, reverse_opcode_mapping)) => {
            info!(
                "Loaded opcode mapping table with {} entries",
                opcode_mapping.iter().filter(|&op| *op != Opcode::UNKNOWN).count()
            );
            (Arc::new(opcode_mapping), Arc::new(reverse_opcode_mapping))
        }
        Err(e) => {
            error!("Can't read opcode mapping file {}: {:?}", &opts.config.display(), e);
            return Err(e);
        }
    };

    info!("Starting the ECS multiverse");
    let global_tx_channel = start_multiverse();

    info!("Starting the web server");
    start_web_server();

    info!("Starting the network server on 127.0.0.1:10001");
    let mut listener = TcpListener::bind("127.0.0.1:10001").await?;

    loop {
        match listener.accept().await {
            Ok((mut socket, addr)) => {
                let thread_channel = global_tx_channel.clone();
                let thread_opcode_mapping = opcode_mapping.clone();
                let thread_reverse_opcode_mapping = reverse_opcode_mapping.clone();

                tokio::spawn(async move {
                    let span = info_span!("socket", %addr);
                    let _enter = span.enter();

                    info!("Incoming connection");
                    match GameSession::new(
                        &mut socket,
                        thread_channel,
                        thread_opcode_mapping,
                        thread_reverse_opcode_mapping,
                    )
                    .await
                    {
                        Ok(mut session) => {
                            let connection = session.connection;
                            match session
                                .handle_connection()
                                .instrument(info_span!("connection", connection = %connection))
                                .await
                            {
                                Ok(_) => info!("Closed connection"),
                                Err(e) => warn!("Error while handling game session: {:?}", e),
                            }
                        }
                        Err(e) => error!("Failed create game session: {:?}", e),
                    }
                });
            }
            Err(e) => error!("Failed to open connection: {:?}", e),
        }
    }
}

// Starts the multiverse on a new thread and returns a channel into the global world.
fn start_multiverse() -> Sender<Arc<Event>> {
    let mut multiverse = Multiverse::new();
    let rx = multiverse.get_global_input_event_channel();

    task::spawn_blocking(move || {
        multiverse.run();
    });

    rx
}

// Starts the web server handling all HTTP/S requests.
fn start_web_server() {
    task::spawn(async {
        web::run().await;
    });
}