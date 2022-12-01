use clap::{CommandFactory, ErrorKind, Parser};
use std::net::SocketAddr;
use std::thread;
use tokio::sync::mpsc;
use url::Url;
use xactserver::pg::PgWatcher;
use xactserver::{Node, NodeId, XactManager, DEFAULT_NODE_PORT, DUMMY_URL};

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
        default_value = "postgresql://localhost:55432",
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
        default_value = "1",
        help = "Numeric id of the current node. Used as an 1-based index of the --nodes list"
    )]
    node_id: NodeId,
}

fn invalid_arg_error(msg: &str) -> ! {
    Args::command().error(ErrorKind::InvalidValue, msg).exit();
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

    // Parse the list of node addresses
    let mut node_addresses: Vec<Url> = args
        .nodes
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
    // Insert a dummy address at the beginning of the list to make the list 1-based
    node_addresses.insert(0, DUMMY_URL.clone());

    // Address to listen to other peers
    let listen_peer = node_addresses
        .get(args.node_id as usize)
        .unwrap_or_else(|| invalid_arg_error("invalid value for '--node-id': out of bound"));

    let (watcher_tx, watcher_rx) = mpsc::channel(100);
    let (node_tx, node_rx) = mpsc::channel(100);

    let mut join_handles = Vec::new();

    let pg_watcher = PgWatcher::new(args.listen_pg, watcher_tx);
    join_handles.push(
        thread::Builder::new()
            .name("pg watcher".into())
            .spawn(move || pg_watcher.thread_main())?,
    );

    let node = Node::new(
        listen_peer.socket_addrs(|| Some(DEFAULT_NODE_PORT))?[0],
        node_tx,
    );
    join_handles.push(
        thread::Builder::new()
            .name("network node".into())
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?
                    .block_on(node.run())?;
                Ok(())
            })?,
    );

    let manager = XactManager::new(
        args.node_id,
        connect_pg,
        node_addresses,
        watcher_rx,
        node_rx,
    );
    join_handles.push(
        thread::Builder::new()
            .name("xact manager".into())
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?
                    .block_on(manager.run())?;
                Ok(())
            })?,
    );

    for handle in join_handles {
        handle.join().unwrap()?;
    }

    Ok(())
}
