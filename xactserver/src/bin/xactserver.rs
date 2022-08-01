use clap::{CommandFactory, ErrorKind, Parser};
use std::net::SocketAddr;
use std::thread;
use tokio::sync::mpsc;
use xactserver::pg::PgWatcher;
use xactserver::{Node, NodeId, XactManager};

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(long, value_parser, default_value = "127.0.0.1:10000")]
    listen_pg: SocketAddr,

    #[clap(long, value_parser, default_value = "127.0.0.1:55432")]
    connect_pg: SocketAddr,

    #[clap(long, value_parser, default_value = "127.0.0.1:23000")]
    nodes: String,

    #[clap(long, value_parser, default_value = "0")]
    node_id: NodeId,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();
    let nodes: Vec<SocketAddr> = args
        .nodes
        .split(",")
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
    let listen_peer = nodes.get(args.node_id as usize).unwrap_or_else(|| {
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

    let manager = XactManager::new(args.node_id, &nodes, args.connect_pg, watcher_rx, node_rx);
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
