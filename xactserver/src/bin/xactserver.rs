use clap::{CommandFactory, ErrorKind, Parser};
use std::net::SocketAddr;
use std::thread;
use tokio::sync::mpsc;
use xactserver::pg::{PgDispatcher, PgWatcher};
use xactserver::{Node, XactServer};

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(long, value_parser, default_value = "127.0.0.1:10000")]
    listen_pg: String,

    #[clap(long, value_parser, default_value = "127.0.0.1:5432")]
    connect_pg: String,

    #[clap(short, long, value_parser)]
    peers: Vec<SocketAddr>,

    #[clap(long, value_parser)]
    id: i32,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();
    let listen_peer = args.peers.get(args.id as usize).unwrap_or_else(|| {
        Args::command()
            .error(ErrorKind::InvalidValue, "id is out of bound")
            .exit();
    });

    let (watcher_tx, watcher_rx) = mpsc::channel(100);
    let (dispatcher_tx, dispatcher_rx) = mpsc::channel(100);
    let (node_tx, node_rx) = mpsc::channel(100);

    let mut join_handles = Vec::new();

    let pg_watcher = PgWatcher::new(&args.listen_pg, watcher_tx);
    join_handles.push(
        thread::Builder::new()
            .name("postgres watcher".into())
            .spawn(move || pg_watcher.thread_main())?,
    );

    let pg_dispatcher = PgDispatcher::new(&args.connect_pg);
    join_handles.push(
        thread::Builder::new()
            .name("postgres dispatcher".into())
            .spawn(move || pg_dispatcher.thread_main(dispatcher_rx))?,
    );

    let node = Node::new(listen_peer, node_tx);
    join_handles.push(
        thread::Builder::new()
            .name("network node".into())
            .spawn(move || node.thread_main())?,
    );

    let xactserver = XactServer::new(&args.peers, dispatcher_tx);
    join_handles.push(
        thread::Builder::new()
            .name("xactserver".into())
            .spawn(move || xactserver.thread_main(watcher_rx, node_rx))?,
    );

    for handle in join_handles {
        handle.join().unwrap()?;
    }

    Ok(())
}
