# SunStorm

SunStorm is geo-partitioned database system based on the Neon database, which used a shared-storage system like Amazon Aurora to disaggregate compute and storage. The SunStorm system consists of three components separated in their own repositories:
+ A modified version of Neon: https://github.com/umd-dslam/sunstorm-neon
+ A modified version of PostgreSQL: https://github.com/umd-dslam/sunstorm-postgres
+ Transaction server (this repository)

Additionally, the following repositories contain the tools and notebooks to run benchmarks on the system:
+ https://github.com/umd-dslam/sunstorm-benchbase
+ https://github.com/umd-dslam/sunstorm-analysis