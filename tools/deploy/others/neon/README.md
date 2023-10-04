# Deploy

Run the following command to deploy.
```
kubectl apply -f deploy/others/neon
```

# Benchmark

You need to set the password to `cloud_admin` for every benchmarking command.

When creating tables, add `-s remotexact=false` to disable loading the remotexact extension.
```
python3 benchmark.py create -s remotexact=false -s password=cloud_admin
```