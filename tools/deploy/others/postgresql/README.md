# Deploy

Run the following command to deploy.
```
kubectl apply -f deploy/others/postgresql
```

# Benchmark

You need to add `-s remotexact=false` when creating the tables to disable loading the remotexact extension.
```
python3 benchmark.py create -s remotexact=false
```