use clap::{App, Arg};
use std::thread;
use tokio::sync::mpsc;
use xactserver::pg::{PgDispatcher, PgWatcher};
use xactserver::{Node, XactServer};

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = App::new("Transaction server")
        .about("Manage the transaction logs")
        .arg(
            Arg::with_name("listen-pg")
                .long("listen-pg")
                .takes_value(true)
                .help("ip:port to listen to for local transactions from a PostgreSQL instance"),
        )
        .arg(
            Arg::with_name("listen-peer")
                .long("listen-peer")
                .takes_value(true)
                .help("ip:port to listen to for connection from other peer servers"),
        )
        .arg(
            Arg::with_name("connect-pg")
                .long("connect-pg")
                .takes_value(true)
                .help("ip:port to connect and send transactions from a remote region to a PostgreSQL instance")
        )
        .arg(
            Arg::with_name("peers")
                .long("peers")
                .use_delimiter(true)
                .takes_value(true),
        )
        .get_matches();

    let listen_pg = args.value_of("listen-pg").unwrap_or("127.0.0.1:10000");
    let listen_peer = args.value_of("listen-peer").unwrap_or("127.0.0.1:23000");
    let connect_pg = args.value_of("connect-pg").unwrap_or("127.0.0.1:5432");
    let peers = args.values_of("peers").unwrap_or_default().collect();

    let (watcher_tx, watcher_rx) = mpsc::channel(100);
    let (dispatcher_tx, dispatcher_rx) = mpsc::channel(100);
    let (node_tx, node_rx) = mpsc::channel(100);

    let mut join_handles = Vec::new();

    let pg_watcher = PgWatcher::new(listen_pg, watcher_tx);
    join_handles.push(
        thread::Builder::new()
            .name("postgres watcher".into())
            .spawn(move || pg_watcher.thread_main())?,
    );

    let pg_dispatcher = PgDispatcher::new(connect_pg);
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

    let xactserver = XactServer::new(peers, dispatcher_tx);
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
