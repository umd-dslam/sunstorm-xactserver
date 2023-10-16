This directory contains the shell scripts for starting a SunStorm cluster from source code. 

Install the Python packages in `requirements.txt`:

```bash
pip install -r tools/requirements.txt
```

+ Start minio:
```
docker compose up -d
```
+ Update the variables at the top of `common`.
+ Run `1-init <num-region>` to initialize the data for the given number of regions.
+ Run `2-start` and `3-compute` to start the cluster.
+ To stop everything, run `4-stop`.
