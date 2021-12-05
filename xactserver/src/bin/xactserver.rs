use clap::{App, Arg};
use std::thread;
use tokio::sync::mpsc;
use xactserver::{LocalLogManager, PgWatcher, RemoteLogsManager, PgDispatcher};

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
   
    
    
    // Create log managers
    let local_log_man = LocalLogManager::new(listen_peer);
    let (remote_log_tx, remote_log_rx) = mpsc::channel(100);
    let remote_logs_man = RemoteLogsManager::new(peers, remote_log_tx);

    // Create a postgres watcher with a channel for sending txn
    // to the local log manager
    let (local_log_tx, local_log_rx) = mpsc::channel(100);
    let pg_watcher = PgWatcher::new(listen_pg, local_log_tx);

    // Create a pg dispatcher to dispatch received remote txn
    let pg_dispatcher = PgDispatcher::new(connect_pg);

    let mut join_handles = Vec::new();

    join_handles.push(
        thread::Builder::new()
            .name("postgres watcher".into())
            .spawn(move || pg_watcher.thread_main())?,
    );

    join_handles.push(
        thread::Builder::new()
            .name("local log manager".into())
            .spawn(move || local_log_man.thread_main(local_log_rx))?,
    );

    join_handles.push(
        thread::Builder::new()
            .name("remote logs manager".into())
            .spawn(move || remote_logs_man.thread_main())?,
    );

    join_handles.push(
        thread::Builder::new()
            .name("postgres dispatcher".into())
            .spawn(move || pg_dispatcher.thread_main(remote_log_rx))?,
    );

    for handle in join_handles {
        handle.join().unwrap()?;
    }

    Ok(())
}
