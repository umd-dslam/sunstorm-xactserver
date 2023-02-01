use clap::{CommandFactory, ErrorKind, Parser};
use hyper::{header::CONTENT_TYPE, Body, Request, Response};
use log::info;
use neon_utils::http::{endpoint::serve_thread_main, error::ApiError};
use prometheus::{Encoder, TextEncoder};
use routerify::Router;
use std::net::SocketAddr;
use std::thread::{self, JoinHandle};
use tokio::sync::mpsc;
use url::Url;
use xactserver::pg::PgWatcher;
use xactserver::{Manager, Node, NodeId, XsMessage, DEFAULT_NODE_PORT};

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(
        long,
        value_parser,
        default_value = "127.0.0.1:10000",
        help = "Address to listen for connections from postgres"
    )]
    listen_pg: SocketAddr,

    #[clap(
        long,
        value_parser,
        default_value = "postgresql://cloud_admin@localhost:55433/postgres",
        help = "Address of postgres to connect to"
    )]
    connect_pg: String,

    #[clap(
        long,
        value_parser,
        default_value = "http://localhost:23000",
        help = "Comma-separated list of addresses of other xact servers in other regions"
    )]
    nodes: String,

    #[clap(
        long,
        value_parser,
        default_value = "0",
        help = "Numeric id of the current node. Used as an 1-based index of the --nodes list"
    )]
    node_id: NodeId,

    #[clap(
        long,
        value_parser,
        default_value = "127.0.0.1:8080",
        help = "Address to listen for http requests"
    )]
    listen_http: SocketAddr,
}

fn invalid_arg_error(msg: &str) -> ! {
    Args::command().error(ErrorKind::InvalidValue, msg).exit();
}

fn parse_node_addresses(nodes: String) -> Vec<Url> {
    let node_addresses: Vec<Url> = nodes
        .split(',')
        .map(|addr| {
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

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();
    if args.node_id == 0 {
        invalid_arg_error("node id is 1-based");
    }

    let connect_pg = Url::parse(&args.connect_pg).unwrap_or_else(|err| {
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

    let nodes = parse_node_addresses(args.nodes);

    let (pg_watcher_handle, watcher_rx) = start_pg_watcher(args.listen_pg)?;
    let (node_handle, node_rx) = start_node(args.node_id, &nodes)?;
    let manager_handle = start_manager(args.node_id, connect_pg, nodes, watcher_rx, node_rx)?;
    let http_server_handle = start_http_server(args.listen_http)?;

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

fn start_node(node_id: NodeId, nodes: &[Url]) -> anyhow::Result<HandleAndReceiver> {
    let (node_tx, node_rx) = mpsc::channel(100);
    let listen_peer = nodes
        .get(node_id)
        .unwrap_or_else(|| invalid_arg_error("invalid value for '--node-id': out of bound"))
        .socket_addrs(|| Some(DEFAULT_NODE_PORT))
        .unwrap_or_else(|e| invalid_arg_error(&e.to_string()))[0];

    info!("Node listening on {}", listen_peer);
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
    nodes: Vec<Url>,
    watcher_rx: mpsc::Receiver<XsMessage>,
    node_rx: mpsc::Receiver<XsMessage>,
) -> anyhow::Result<JoinHandle<Result<(), anyhow::Error>>> {
    let manager = Manager::new(node_id, connect_pg, nodes, watcher_rx, node_rx);

    Ok(thread::Builder::new()
        .name("xact manager".into())
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
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
