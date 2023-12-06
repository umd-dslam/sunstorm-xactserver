# Deployment & Benchmarking

The servers can be deployed on Kubernetes clusters or directly on the EC2 instances. However, it is recommended that the benchmarking tools are deployed on Kubernetes to simplify the control of the clients across different regions.

## Table of Contents
- [One-time network configuration](#one-time-network-configuration)
    - [Set up VPC peering](#set-up-vpc-peering)
    - [Create subnets](#create-subnets)
    - [Create security groups](#create-security-groups)
- [Create EKS clusters](#create-eks-clusters)
    - [Specify EKS configurations](#specify-eks-configurations)
    - [Specify AWS regions](#specify-aws-regions)
    - [Run the EKS clusters creation script](#run-the-eks-clusters-creation-script)
- [Deploy the servers](#deploy-the-servers)
    - [On Kubernetes](#on-kubernetes)
    - [On EC2 instances](#on-ec2-instances)
- [Useful commands to manage the EKS clusters](#useful-commands-to-manage-the-eks-clusters)
    - [Monitoring tool](#monitoring-tool)
    - [Contexts](#contexts)
    - [Resources](#resources)
    - [Logs](#logs)
    - [Port-forwarding](#port-forwarding)
- [Run the benchmark](#run-the-benchmark)
    - [If the servers are deployed direclty on the EC2 instances](#if-the-servers-are-deployed-direclty-on-the-ec2-instances)
    - [Create the benchmark schema](#create-the-benchmark-schema)
    - [Load the data](#load-the-data)
    - [Execute the benchmark](#execute-the-benchmark)
- [Run the experiments](#run-the-experiments)
- [Clean up](#clean-up)

## One-time network configuration

### Set up VPC peering

For pods to communicate across separate Kubernetes clusters, the VPCs in all regions need to be peered. Network traffic can then be routed between the VPCs. For more information about VPC peering, see the [AWS documentation](https://docs.aws.amazon.com/vpc/latest/peering/what-is-vpc-peering.html).

1. Open the [Amazon VPC console](https://console.aws.amazon.com/vpc/) and create a new VPC for each region. Specify unique and **non-overlapping** IP ranges for the VPCs. You can use the following example:

    | region | CIDR block |
    |--------|------------|
    | us-east-1 | 10.0.0.0/16 |
    | us-east-2 | 10.1.0.0/16 |
    | us-west-1 | 10.2.0.0/16 |
    | us-west-2 | 10.3.0.0/16 |

1. Note the ID of the VPC in each region. The VPC ID is found in the section called Your VPCs.

1. Navigate to the Peering Connections section and [create a VPC peering connection](https://docs.aws.amazon.com/vpc/latest/peering/create-vpc-peering-connection.html#create-vpc-peering-connection-local) between each of the regions. When you create a peering connection, you will first select a requester VPC in the current region and then an accepter VPC in another region, specified by pasting the VPC ID.   

1. To complete the VPC peering connections, switch to each destination region and [accept the pending connection](https://docs.aws.amazon.com/vpc/latest/peering/create-vpc-peering-connection.html#accept-vpc-peering-connection) in the VPC console.

1. For every region, navigate to the Route Tables section and find the route table of the newly created VPC. [Update this route table](https://docs.aws.amazon.com/vpc/latest/peering/vpc-peering-routing.html) with new entries that point traffic to the other peered VPCs. The **Destination** should be the CIDR block of a destination region, and the **Target** should be the VPC peering connection between the current and the destination region.

### Create subnets

Create at least one subnet in each VPC. Open the EKS yaml file of the respective region in the `deploy/eks` directory (e.g. `deploy/eks/us-east-1.yaml`) and put in the subnet IDs in the `vpc.subnets.public` field. For example, the subnets of the VPC in the region `us-east-1` above could be:

| Subnet ID | CIDR block | Availability Zone |
|-----------|------------|-------------------|
| subnet-01ffdf1e8d2d11bce | 10.0.0.0/24 | us-east-1a |
| subnet-0f2ae7a9189cd2786 | 10.0.1.0/24 | us-east-1b |
| subnet-0346cab72850ef316 | 10.0.2.0/24 | us-east-1c |

The `deploy/eks/us-east-1.yaml` would contain the following:

### Create security groups

For every region, create a security group that allows all traffic from other regions. In the example above, the security group will have one inbound rule where the **Source** is `10.0.0.0/8` so that we allow traffic from all other regions.

Copy the ID of this security group and use it in the `securityGroups` fields in the EKS yaml file (e.g. `deploy/eks/us-east-1.yml`) of the corresponding region.


```yaml
vpc:
  subnets:
    public:
      us-east-1a: { id: subnet-01ffdf1e8d2d11bce }
      us-east-1b: { id: subnet-0f2ae7a9189cd2786 }
      us-east-1c: { id: subnet-0346cab72850ef316 }
``````

## Create EKS clusters

### Specify EKS configurations

For every region, adjust the configurations in `deploy/helm-eks/values.yaml` as needed. If you are not deploying the servers on Kubernetes, only specify the instance type in `instance_types.client` and comment out other components.

The full schema of this config file can be found [here](https://eksctl.io/usage/schema/#managedNodeGroups).

### Specify AWS regions

Specify the regions and global region in the `deploy/main.yaml` file. The region information in this file will be the source of truth for the rest of the deployment process.

### Run the EKS clusters creation script

Run the following command to create the EKS clusters:

```
python3 eks.py create
```

It may take around 15 minutes for the clusters to be created. You can check the status of the clusters in the log files in `deploy/eks` or the AWS console.

## Deploy the servers

Every region must have at least one machine that runs the server. Make sure that the EBS volume has enough IOPS and throughput to handle the workload (e.g. we use the `io2` volume type to run our experiments). If deploying directly on the EC2 instances, remember to add one extra machine to one of the regions. This machine, called "hub" hereafter, will run the "global" region and other utilities (e.g. minio, promethus, and grafana).

### On Kubernetes

Adjust the parameters in the file `deploy/helm-neon/values.yaml` as needed (note that the `regions` field will be overwritten by the tool so can be ignored), then run the following command to deploy the system:

```
python3 deploy.py 
```

On first run, the DNS configmap installation stage may take a few minutes to wait for the load balancer to be ready.

This tool also provides options to skip some stages in the development process if needed. Run `python3 deploy.py -h` to see the available options. For example, sometimes when some of the spot instances are terminated, you need to redeploy the system only without reinstalling the DNS configmap. In this case, you can run:

```
python3 deploy.py --skip-before neon
```

### On EC2 instances 

The following instructions assume the machines are running Ubuntu 22.04.

The shell scripts for starting the servers on EC2 instances are located in `deploy/shell`. First of all, install the Python packages in `requirements.txt` on every machine:

```bash
pip install -r tools/requirements.txt
```

#### Start the Docker containers

Optional: Mount an EBS volume to store the metrics at `/media/data`.

Run the following commands (even if you don't mount the EBS volume):
```
sudo mkdir -p /media/data/minio /media/data/prometheus /media/data/grafana
sudo chown -R 1000:1000 /media/data/minio
sudo chown -R 65534:65534 /media/data/prometheus
sudo chown -R 472:0 /media/data/grafana
```

Add the private IP addresses of the servers to the `pageserver`, `safekeeper`, and `xactserver` jobs in `shell/prometheus.yml`. 

Add the private IP addresses of the clients to the `pushgateway` job in `shell/prometheus.yml`

Start the Docker containers 
```
docker compose up -d
```

#### Start the servers

Update the variables at the top of `shell/common` file on all instances to match with the information of the EC2 instances (e.g. private IP addresses).

On the hub, run `1-init <number-of-regions>` to initialize the data for the given number of regions (excluding the global region). If you need to re-run this script, make sure to remove the generated `init` directory before that.

On every machine (including the hub), run `2-start` and `3-compute` to start the components of server. Run these scripts with the `-h` flag to see more options.

To stop everything, run `4-stop`.


## Useful commands to manage the EKS clusters

### Monitoring tool

Use the `monitor.py` script to monitor the pods across the regions specified in `main.yaml`
```
python3 tools/monitor.py status
```

### Contexts

To control the cluster in a particular region, you need to set the context of `kubectl` to that region. To see the existing contexts, run:

```
kubectl config get-contexts
```
Example output:
```
CURRENT   NAME                              CLUSTER                    AUTHINFO                          NAMESPACE
*         ctring@neon.us-east-1.eksctl.io   neon.us-east-1.eksctl.io   ctring@neon.us-east-1.eksctl.io   
          ctring@neon.us-east-2.eksctl.io   neon.us-east-2.eksctl.io   ctring@neon.us-east-2.eksctl.io   
          ctring@neon.us-west-1.eksctl.io   neon.us-west-1.eksctl.io   ctring@neon.us-west-1.eksctl.io  
```

From this point on, we will specify the context of the command using the `--context` flag. Remember to replace it with the context of your cluster.

### Resources

Get all deployments in `us-east-1`. The `-A` flag is used to get resources from all namespaces. If you want to get resources from a specific namespace, replace `-A` with `-n <namespace>`.

```
kubectl get deploy -A --context ctring@neon.us-east-1.eksctl.io 
```
```
NAMESPACE     NAME             READY   UP-TO-DATE   AVAILABLE   AGE
global        compute          1/1     1            1           6m9s
global        minio            1/1     1            1           6m9s
global        pageserver       1/1     1            1           6m9s
global        storage-broker   1/1     1            1           6m9s
global        xactserver       1/1     1            1           6m9s
kube-system   coredns          2/2     2            2           20m
us-east-1     compute          1/1     1            1           6m6s
us-east-1     pageserver       1/1     1            1           6m6s
us-east-1     xactserver       1/1     1            1           6m6s                                                 app=xactserver
```

You can get other types of resources by replacing `deploy` with the corresponding resource type (e.g. `pod`, `svc`, etc.).

You can also add `-o wide` to get more information about the resources.

It is useful to see the status of the pods to make sure every thing is up and running, for example:

```
kubectl get pod -A -o wide --context ctring@neon.us-east-1.eksctl.io
```

### Logs

Get the logs of the `xactserver` deployment in `us-east-1`

```
kubectl logs deploy/xactserver -n us-east-1 --context ctring@neon.us-east-1.eksctl.io
```
```
[2023-08-15T18:55:56Z INFO  xactserver] Listening to PostgreSQL on 0.0.0.0:10000
[2023-08-15T18:55:56Z INFO  xactserver] Listening to peers on 0.0.0.0:23000
[2023-08-15T18:55:56Z INFO  utils::http::endpoint] Starting an HTTP endpoint at 0.0.0.0:8080
```

Add `-f` if you want to follow the logs.

### Port forwarding

It is useful to be able to connect to minio or the compute node from your local machine. To do so, run the following command to forward the port of the resource to your local machine:

```
kubectl port-forward svc/minio -n global 9000 9001 --context ctring@neon.us-east-1.eksctl.io
```

```
kubectl port-forward svc/compute -n us-east-1 55433 --context ctring@neon.us-east-1.eksctl.io
```

## Run the benchmark

Set the parameters for the benchmark appropriately in `deploy/helm-benchbase/values.yaml`. Remember to change `minio` to point to the hub if you deploy the servers without Kubernetes. Note that you can change any of these values later in the command line.

### If the servers are deployed direclty on the EC2 instances

Run the following commands to create necessary resources to run the benchmark on Kubernetes:
```
python3 tools/deploy.py --skip-before namespace --skip-after namespace
python3 tools/deploy.py --skip-before pushgateway --skip-after pushgateway
```

Make sure that the `target_address_and_database` field is set to the URL to connect to the databases in every region, including the global region.

### Create the benchmark schema

```
python3 benchmark.py create
```

On first run, there may be error messages about configmaps or jobs not found for deletion. This is normal and can be ignored.

Check the logs of the schema creation job 

```
kubectl logs job/create-load -n global --context ctring@neon.us-east-1.eksctl.io
```

### Load the data

```
python3 benchmark.py load
```

Check the logs of the data loading job

```
kubectl logs job/create-load -n global --context ctring@neon.us-east-1.eksctl.io
```

### Execute the benchmark

```
python3 benchmark.py execute
```

Check the logs of the execution job

```
kubectl logs job/execute -n us-east-1 --context ctring@neon.us-east-1.eksctl.io
```

The results can be found on Minio.

You can overwrite the values directly in the command line:

```
python3 tools/benchmark.py execute -s tag=hot0mr0 -s hot.mr=0 -s hot.hot=0
python3 tools/benchmark.py execute -s tag=hot0mr5 -s hot.mr=5 -s hot.hot=0
python3 tools/benchmark.py execute -s tag=hot0mr10 -s hot.mr=10 -s hot.hot=0
```

## Run the experiments
The experiment script automates running the benchmark over multiple combinations of the parameters. 

If the servers are deployed on Kubernetes, forward the Minio port to localhost
```
port-forward svc/hub 9000 9001 -n global --context ctring@neon.us-east-1.eksctl.io
```

Run the experiment script. The following command would run the ycsb-throughput experiment (see the configurations under the `experiments` directory), and download the results from minio to the `~/data/sunstorm/ycsb/throughput` directory. The `logs-per-sec` flag is used to reduce the logging rate on the terminal while running the experiment.

```
python3 tools/experiment.py           \
  --logs-per-sec 50                   \
  --minio <address-to-minio>:9000     \
  -o ~/data/sunstorm/ycsb/throughput  \
  ycsb-throughput   
```

The progress of the experiment is saved under the workspace directory. You can modify the progress files to re-run or skip a data point. Use `-h` to see more options for the `experiment.py` script.

## Clean up

Destroy the EKS clusters:

```
python3 eks.py delete
```