use clap::{error::ErrorKind, CommandFactory, Parser};
use hyper::{header::CONTENT_TYPE, Body, Request, Response};
use log::info;
use neon_utils::http::{endpoint::serve_thread_main, error::ApiError};
use prometheus::{Encoder, TextEncoder};
use routerify::Router;
use std::net::SocketAddr;
use std::thread::{self, JoinHandle};
use std::{panic, process};
use tokio::sync::mpsc;
use url::Url;
use xactserver::pg::PgWatcher;
use xactserver::{Manager, Node, NodeId, XsMessage, DEFAULT_NODE_PORT, DUMMY_URL};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(
        long,
        value_parser,
        default_value = "127.0.0.1:10000",
        help = "Address to listen for connections from postgres"
    )]
    listen_pg: SocketAddr,

    #[arg(
        long,
        value_parser,
        default_value = "postgresql://cloud_admin@localhost:55433/postgres",
        help = "Address of postgres to connect to"
    )]
    connect_pg: String,

    #[arg(
        long,
        short,
        default_value = "128",
        help = "Maximum size of the postgres connection pool"
    )]
    max_pg_conn_pool_size: u32,

    #[arg(
        long,
        value_parser,
        default_value = "http://localhost:23000",
        help = "Comma-separated list of addresses of other xact servers in other regions"
    )]
    nodes: String,

    #[arg(
        long,
        value_parser,
        default_value = "0",
        help = "Numeric id of the current node. Used as an 0-based index of the --nodes list"
    )]
    node_id: NodeId,

    #[arg(
        long,
        value_parser,
        default_value = "127.0.0.1:8080",
        help = "Address to listen for http requests"
    )]
    listen_http: SocketAddr,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let orig_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        orig_hook(panic_info);
        process::exit(1);
    }));

    let cli = Cli::parse();

    let connect_pg = Url::parse(&cli.connect_pg).unwrap_or_else(|err| {
        invalid_arg_error(&format!(
            "unable to parse postgres url in '--connect-pg': {}",
            err
        ))
    });

    if !connect_pg.scheme().eq("postgresql") {
        invalid_arg_error(&format!(
            "invalid scheme '{}' in '--connect-pg'. Must be 'postgresql'",
            connect_pg.scheme()
        ));
    }

    let nodes = parse_node_addresses(cli.nodes);
    let listen_peers_url = nodes
        .get(cli.node_id)
        .unwrap_or_else(|| invalid_arg_error("invalid value for '--node-id': out of bound"));

    let (pg_watcher_handle, watcher_rx) = start_pg_watcher(cli.listen_pg)?;
    let (node_handle, node_rx) = start_peer_listener(listen_peers_url)?;
    let manager_handle = start_manager(
        cli.node_id,
        connect_pg,
        cli.max_pg_conn_pool_size,
        nodes,
        watcher_rx,
        node_rx,
    )?;
    let http_server_handle = start_http_server(cli.listen_http)?;

    for handle in [
        pg_watcher_handle,
        node_handle,
        manager_handle,
        http_server_handle,
    ] {
        handle.join().unwrap()?;
    }

    Ok(())
}

fn invalid_arg_error(msg: &str) -> ! {
    Cli::command().error(ErrorKind::InvalidValue, msg).exit();
}

fn parse_node_addresses(nodes: String) -> Vec<Url> {
    let node_addresses: Vec<Url> = nodes
        .split(',')
        .map(|addr| {
            if addr.to_lowercase() == "dummy" {
                return DUMMY_URL.clone();
            }

            let node_url = Url::parse(addr).unwrap_or_else(|err| {
                invalid_arg_error(&format!("invalid value '{}' in '--nodes': {}", addr, err))
            });

            let scheme = node_url.scheme();
            if !["http", "https"].contains(&scheme) {
                invalid_arg_error(&format!(
                    "invalid scheme '{}' in '--nodes'. Must be either http or https",
                    scheme
                ));
            }

            node_url
        })
        .collect();
    node_addresses
}

type HandleAndReceiver = (
    JoinHandle<Result<(), anyhow::Error>>,
    mpsc::Receiver<XsMessage>,
);

fn start_pg_watcher(listen_pg: SocketAddr) -> anyhow::Result<HandleAndReceiver> {
    let (watcher_tx, watcher_rx) = mpsc::channel(100);
    info!("Listening to PostgreSQL on {}", listen_pg);
    let pg_watcher = PgWatcher::new(listen_pg, watcher_tx);
    let handle = thread::Builder::new()
        .name("pg watcher".into())
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(pg_watcher.run())?;
            Ok(())
        })?;

    Ok((handle, watcher_rx))
}

fn start_peer_listener(listen_url: &Url) -> anyhow::Result<HandleAndReceiver> {
    let (node_tx, node_rx) = mpsc::channel(100);
    let listen_peer = listen_url
        .socket_addrs(|| Some(DEFAULT_NODE_PORT))
        .unwrap_or_else(|e| invalid_arg_error(&e.to_string()))[0];

    info!("Listening to peers on {}", listen_peer);
    let node = Node::new(listen_peer, node_tx);
    let handle = thread::Builder::new()
        .name("network node".into())
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(node.run())?;
            Ok(())
        })?;

    Ok((handle, node_rx))
}

fn start_manager(
    node_id: NodeId,
    connect_pg: Url,
    max_pg_conn_pool_size: u32,
    nodes: Vec<Url>,
    watcher_rx: mpsc::Receiver<XsMessage>,
    node_rx: mpsc::Receiver<XsMessage>,
) -> anyhow::Result<JoinHandle<Result<(), anyhow::Error>>> {
    let manager = Manager::new(
        node_id,
        connect_pg,
        max_pg_conn_pool_size,
        nodes,
        watcher_rx,
        node_rx,
    );

    Ok(thread::Builder::new()
        .name("xact manager".into())
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(manager.run())?;
            Ok(())
        })?)
}

fn start_http_server(
    listen_http: SocketAddr,
) -> anyhow::Result<JoinHandle<Result<(), anyhow::Error>>> {
    let listener = std::net::TcpListener::bind(listen_http)?;
    let router = Router::builder().get("/metrics", prometheus_metrics_handler);

    Ok(thread::Builder::new()
        .name("xact manager".into())
        .spawn(move || {
            serve_thread_main(
                router,
                listener,
                std::future::pending(), // never shut down
            )
        })?)
}

async fn prometheus_metrics_handler(_req: Request<Body>) -> Result<Response<Body>, ApiError> {
    let mut buffer = vec![];
    let encoder = TextEncoder::new();

    let metrics = prometheus::gather();
    encoder.encode(&metrics, &mut buffer).unwrap();

    let response = Response::builder()
        .status(200)
        .header(CONTENT_TYPE, encoder.format_type())
        .body(Body::from(buffer))
        .unwrap();

    Ok(response)
}
