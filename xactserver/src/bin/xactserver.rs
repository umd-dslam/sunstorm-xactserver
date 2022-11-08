use clap::{CommandFactory, ErrorKind, Parser};
use std::net::SocketAddr;
use std::thread;
use tokio::sync::mpsc;
use xactserver::pg::PgWatcher;
use xactserver::{Node, NodeId, XactManager, DUMMY_ADDRESS};

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
        default_value = "127.0.0.1:55432",
        help = "Address of postgres to connect to"
    )]
    connect_pg: SocketAddr,

    #[clap(
        long,
        value_parser,
        default_value = "127.0.0.1:23000",
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

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();
    if args.node_id == 0 {
        Args::command()
            .error(ErrorKind::InvalidValue, "node id is 1-based")
            .exit();
    }

    // Parse the list of node addresses
    let mut node_addresses: Vec<SocketAddr> = args
        .nodes
        .split(',')
        .map(|addr| {
            addr.parse().unwrap_or_else(|err| {
                Args::command()
                    .error(
                        ErrorKind::InvalidValue,
                        format!("Invalid value '{}' for '--nodes': {}", addr, err),
                    )
                    .exit();
            })
        })
        .collect();
    // Insert a dummy address at the beginning of the list to make the list 1-based
    node_addresses.insert(0, *DUMMY_ADDRESS);

    // Address to listen to other peers
    let listen_peer = node_addresses
        .get(args.node_id as usize)
        .unwrap_or_else(|| {
            Args::command()
                .error(
                    ErrorKind::InvalidValue,
                    "Invalid value for '--node-id': out of bound",
                )
                .exit();
        });

    let (watcher_tx, watcher_rx) = mpsc::channel(100);
    let (node_tx, node_rx) = mpsc::channel(100);

    let mut join_handles = Vec::new();

    let pg_watcher = PgWatcher::new(args.listen_pg, watcher_tx);
    join_handles.push(
        thread::Builder::new()
            .name("pg watcher".into())
            .spawn(move || pg_watcher.thread_main())?,
    );

    let node = Node::new(listen_peer, node_tx);
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
        args.connect_pg,
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
