# Slogora

## Run locally

Run `make` to build.

Start transaction server:

```
$ target/debug/xactserver
```

Initialize the database:

```
$ export PGDATA=$HOME/data/postgresql
$ mkdir -p $PGDATA
$ tmp_install/bin/initdb
```

Start PostgreSQL:

```
$ postgres -c shared_preload_libraries=remotexact
```
