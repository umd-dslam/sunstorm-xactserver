use clap::{App, Arg};
use std::thread;
use xactserver::PgWatcher;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info")
    ).init();

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

    let pg_watcher = PgWatcher::new(&listen_pg);

    thread::Builder::new()
        .name("postgres watcher".into())
        .spawn(move || pg_watcher.thread_main())?
        .join()
        .unwrap()?;

    Ok(())
}
