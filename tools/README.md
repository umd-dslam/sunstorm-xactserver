# EKS Deployment Instructions

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

### Create security groups

For every region, create a security group that allows all traffic from other regions. In the example above, the security group will have one inbound rule where the **Source** is `10.0.0.0/8` so that we allow traffic from all other regions.

Copy the ID of this security group and use it in the `securityGroups` fields in the EKS yaml file (e.g. `deploy/eks/us-east-1.yml`) of the corresponding region.


### Create subnets

Create at least one subnet in each VPC. Open the EKS yaml file of the respective region in the `deploy/eks` directory (e.g. `deploy/eks/us-east-1.yaml`) and put in the subnet IDs in the `vpc.subnets.public` field. For example, the subnets of the VPC in the region `us-east-1` above could be:

| Subnet ID | CIDR block | Availability Zone |
|-----------|------------|-------------------|
| subnet-01ffdf1e8d2d11bce | 10.0.0.0/24 | us-east-1a |
| subnet-0f2ae7a9189cd2786 | 10.0.1.0/24 | us-east-1b |
| subnet-0346cab72850ef316 | 10.0.2.0/24 | us-east-1c |

The `deploy/eks/us-east-1.yaml` would contain the following:

```yaml
vpc:
  subnets:
    public:
      us-east-1a: { id: subnet-01ffdf1e8d2d11bce }
      us-east-1b: { id: subnet-0f2ae7a9189cd2786 }
      us-east-1c: { id: subnet-0346cab72850ef316 }
``````

## Create EKS clusters

### Specify EC2 instances

For every region, adjust the parameters under `managedNodeGroups` of the EKS yaml file as needed (e.g. `instanceType`, `desiredCapacity`, etc.). Remember to specify extra instances in the AWS region that holds the global resources.

The full schema of this config file can be found [here](https://eksctl.io/usage/schema/#managedNodeGroups).

### Specify AWS regions

Specify the regions and global region in the `deploy/regions.yaml` file. The regions information in this file will be the source of truth for the rest of the deployment process.

### Run the EKS clusters creation script

Run the following command to create the EKS clusters:

```
python3 eks.py create
```

It may take around 15 minutes for the clusters to be created. You can check the status of the clusters in the log files in `deploy/eks` or the AWS console.

## Deploy the system

Adjust the parameters in the file `deploy/helm-neon/values.yaml` as needed (note that the `regions` field will be overwritten by the tool so can be ignored), then run the following command to deploy the system:

```
python3 deploy.py 
```

On first run, the DNS configmap installation stage may take a few minutes to wait for the load balancer to be ready.

This tool also provides options to skip some stages in the development process if needed. Run `python3 deploy.py -h` to see the available options. For example, sometimes when some of the spot instances are terminated, you need to redeploy the system only without reinstalling the DNS configmap. In this case, you can run:

```
python3 deploy.py --skip-before neon
```

# Useful commands to manage the cluster

## Contexts

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

## Resources

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

## Logs

Get the logs of the `xactserver` deployment in `us-east-1`

```
kubectl logs deploy/xactserver --context ctring@neon.us-east-1.eksctl.io -n us-east-1
```
```
[2023-08-15T18:55:56Z INFO  xactserver] Listening to PostgreSQL on 0.0.0.0:10000
[2023-08-15T18:55:56Z INFO  xactserver] Listening to peers on 0.0.0.0:23000
[2023-08-15T18:55:56Z INFO  utils::http::endpoint] Starting an HTTP endpoint at 0.0.0.0:8080
```

Add `-f` if you want to follow the logs.

## Port forwarding

It is useful to be able to connect to minio or the compute node from your local machine. To do so, run the following command to forward the port of the resource to your local machine:

```
kubectl port-forward svc/minio -n global 9000 9001 --context ctring@neon.us-east-1.eksctl.io
```

```
kubectl port-forward svc/compute -n us-east-1 55433 --context ctring@neon.us-east-1.eksctl.io
```


# Run the benchmark

Set the parameters for the benchmark appropriately in `deploy/helm-benchbase/values.yaml`. Note that you can change any of these values later in the command line.

## Create the benchmark schema

```
python3 benchmark.py create
```

On first run, there may be error messages about configmaps or jobs not found for deletion. This is normal and can be ignored.

Check the logs of the schema creation job 

```
kubectl logs job/create-load -n global --context ctring@neon.us-east-1.eksctl.io
```

## Load the data

```
python3 benchmark.py load
```

Check the logs of the data loading job

```
kubectl logs job/create-load -n global --context ctring@neon.us-east-1.eksctl.io
```

## Execute the benchmark

```
python3 benchmark.py execute
```

Check the logs of the execution job

```
kubectl logs job/execute -n us-east-1 --context ctring@neon.us-east-1.eksctl.io
```

The results can be found on Minio.

# Clean up

Destroy the EKS clusters:

```
python3 eks.py delete
```