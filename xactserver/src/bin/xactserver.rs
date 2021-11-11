use clap::{App, Arg};
use std::thread;
use tokio::sync::mpsc;
use xactserver::{LocalLogManager, PgWatcher};

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
        .get_matches();

    let listen_pg = String::from(args.value_of("listen-pg").unwrap_or("127.0.0.1:8888"));

    // Create local log manager
    let local_log_man = LocalLogManager::new();

    // Create a channel for sending txn from the postgres watcher
    // to the local log manager
    let (local_log_tx, local_log_rx) = mpsc::channel(32);
    let pg_watcher = PgWatcher::new(&listen_pg, local_log_tx);

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

    for handle in join_handles {
        handle.join().unwrap()?;
    }

    Ok(())
}
