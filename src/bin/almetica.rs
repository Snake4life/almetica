#![warn(clippy::all)]
use std::collections::HashMap;
use std::path::PathBuf;
use std::process;
use std::sync::Arc;

use async_std::prelude::*;
use async_std::sync::Sender;
use async_std::task::{self, JoinHandle};
use clap::{App, Arg, ArgMatches};
use sqlx::PgPool;
use tokio::runtime::Runtime;
use tracing::{error, info, warn};
use tracing_log::LogTracer;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};
use tracing_subscriber::fmt::Layer;
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::Registry;

use almetica::config::{read_configuration, Configuration};
use almetica::dataloader::load_opcode_mapping;
use almetica::ecs::event::Event;
use almetica::ecs::world::Multiverse;
use almetica::model::embedded::migrations;
use almetica::model::entity::Account;
use almetica::model::repository::account;
use almetica::model::PasswordHashAlgorithm;
use almetica::protocol::opcode::Opcode;
use almetica::webserver;
use almetica::Result;
use almetica::{networkserver, Error};
use chrono::Utc;

#[async_std::main]
async fn main() {
    let matches = App::new("almetica")
        .version("0.0.2")
        .author("Almetica <almetica@protonmail.com>")
        .about("Custom server implementation for the game TERA")
        .arg(
            Arg::with_name("config")
                .short('c')
                .long("config")
                .value_name("FILE")
                .help("Sets a custom config file")
                .default_value("config.yaml")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("log")
                .short('l')
                .long("log")
                .value_name("LEVEL")
                .help("Sets the log level")
                .default_value("INFO")
                .possible_values(&["ERROR", "WARN", "INFO", "DEBUG", "TRACE"])
                .takes_value(true),
        )
        .subcommand(App::new("run").about("Starts the game server"))
        .subcommand(
            App::new("create-account")
                .about("Creates an account")
                .arg(
                    Arg::with_name("name")
                        .short('n')
                        .long("name")
                        .help("name of the account")
                        .required(true)
                        .takes_value(true),
                )
                .arg(
                    Arg::with_name("password")
                        .short('p')
                        .long("password")
                        .help("password of the account")
                        .required(true)
                        .takes_value(true),
                ),
        )
        .get_matches();

    init_logging(&matches);

    if let Err(e) = run_command(&matches).await {
        error!("Error while executing program: {:?}", e);
        process::exit(1);
    }
}

fn init_logging(matches: &ArgMatches) {
    let level = match matches.value_of("log").unwrap_or_default() {
        "ERROR" => LevelFilter::ERROR,
        "WARN" => LevelFilter::WARN,
        "INFO" => LevelFilter::INFO,
        "DEBUG" => LevelFilter::DEBUG,
        "TRACE" => LevelFilter::TRACE,
        _ => LevelFilter::INFO,
    };

    let fmt_layer = Layer::default().with_target(true);
    let filter_layer = EnvFilter::from_default_env()
        .add_directive(level.into())
        .add_directive("async_std::task::builder=warn".parse().unwrap())
        .add_directive("async_std::task::block_on=warn".parse().unwrap());

    let subscriber = Registry::default().with(filter_layer).with(fmt_layer);
    tracing::subscriber::set_global_default(subscriber).unwrap();
    LogTracer::init().unwrap();
}

async fn run_command(matches: &ArgMatches) -> Result<()> {
    info!("Reading configuration file");
    let config_str = matches.value_of("config").unwrap_or("config.yaml");
    let path = PathBuf::from(config_str);
    let config = match read_configuration(&path) {
        Ok(c) => c,
        Err(e) => {
            error!("Can't read configuration file {:?}: {:?}", path, e);
            return Err(e);
        }
    };

    if let Some(matches) = matches.subcommand_matches("run") {
        start_server(matches, &config).await?;
    } else if let Some(matches) = matches.subcommand_matches("create-account") {
        create_account(matches, &config).await?;
    }
    Ok(())
}

async fn start_server(_matches: &ArgMatches, config: &Configuration) -> Result<()> {
    info!("Reading opcode mapping file");
    let (opcode_mapping, reverse_opcode_mapping) = match load_opcode_mapping(&config.data.path) {
        Ok((opcode_mapping, reverse_opcode_mapping)) => {
            info!(
                "Loaded opcode mapping table with {} entries",
                opcode_mapping
                    .iter()
                    .filter(|&op| *op != Opcode::UNKNOWN)
                    .count()
            );
            (opcode_mapping, reverse_opcode_mapping)
        }
        Err(e) => {
            error!(
                "Can't read opcode mapping file {:?}: {:?}",
                &config.data.path, e
            );
            return Err(e);
        }
    };

    info!("Running database migrations");
    run_db_migrations(&config)?;

    info!("Creating database pool");
    let pool = sqlx_pool(&config).await?;

    info!("Starting the ECS multiverse");
    let (multiverse_handle, global_tx_channel) = start_multiverse(config.clone(), pool.clone());

    info!("Starting the web server");
    let web_handle = start_web_server(pool, config.clone());

    info!("Starting the network server");
    let network_handle = start_network_server(
        global_tx_channel,
        opcode_mapping,
        reverse_opcode_mapping,
        config.clone(),
    );

    let (_, err) = multiverse_handle
        .join(web_handle)
        .join(network_handle)
        .await;
    if let Err(e) = err {
        error!("Can't shutdown server gracefully: {:?}", e);
    }

    Ok(())
}

/// Performs the database migrations
fn run_db_migrations(config: &Configuration) -> Result<()> {
    // FIXME: Use sqlx once refinery adds support for it or we implement our own migration framework.
    let mut rt = Runtime::new()?;
    rt.block_on(async {
        let db_conf = tokio_postgres_config(&config);
        let (mut client, connection) = db_conf.connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                error!("connection error: {}", e);
            }
        });
        migrations::runner().run_async(&mut client).await.unwrap();
    });
    Ok(())
}

/// Starts the multiverse on a new thread and returns a channel into the global world.
fn start_multiverse(config: Configuration, pool: PgPool) -> (JoinHandle<()>, Sender<Arc<Event>>) {
    let mut multiverse = Multiverse::new();
    let rx = multiverse.get_global_input_event_channel();

    let join_handle = task::spawn_blocking(move || {
        multiverse.run(pool, config);
    });

    (join_handle, rx)
}

/// Starts the web server handling all HTTP requests.
fn start_web_server(pool: PgPool, config: Configuration) -> JoinHandle<()> {
    task::spawn(async {
        if let Err(e) = webserver::run(pool, config).await {
            error!("Can't run the web server: {:?}", e);
        };
    })
}

/// Starts the network server that handles all TCP game client connections.
fn start_network_server(
    global_channel: Sender<Arc<Event>>,
    map: Vec<Opcode>,
    reverse_map: HashMap<Opcode, u16>,
    config: Configuration,
) -> JoinHandle<Result<()>> {
    task::spawn(async { networkserver::run(global_channel, map, reverse_map, config).await })
}

fn tokio_postgres_config(config: &Configuration) -> tokio_postgres::Config {
    let mut c = tokio_postgres::Config::new();
    c.host(&config.database.hostname);
    c.port(config.database.port);
    c.user(&config.database.username);
    c.password(&config.database.password);
    c.dbname(&config.database.database);
    c
}

async fn sqlx_pool(config: &Configuration) -> Result<PgPool> {
    Ok(PgPool::new(sqlx_config(config).as_ref()).await?)
}

fn sqlx_config(config: &Configuration) -> String {
    format!(
        "postgres://{}:{}@{}:{}/{}",
        config.database.username,
        config.database.password,
        config.database.hostname,
        config.database.port,
        config.database.database
    )
}

async fn create_account(matches: &ArgMatches, config: &Configuration) -> Result<()> {
    let mut conn = sqlx_pool(&config).await?.acquire().await?;

    let account_name = matches.value_of("name").unwrap_or_default();
    let password = matches.value_of("password").unwrap_or_default();

    match account::get_by_name(&mut conn, account_name).await {
        Err(Error::Sqlx(sqlx::Error::RowNotFound)) => {
            let acc = account::create(
                &mut conn,
                &Account {
                    id: -1,
                    name: account_name.to_string(),
                    password: password.to_string(),
                    algorithm: PasswordHashAlgorithm::Argon2,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
            )
            .await?;
            info!("Created account {} with ID {}", acc.name, acc.id);
        }
        Err(e) => {
            return Err(e);
        }
        Ok(acc) => {
            error!("Account {} already exists with ID {}", acc.name, acc.id);
        }
    }
    Ok(())
}
