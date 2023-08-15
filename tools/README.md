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

For each region, create a security group that allows all traffic from other regions. In the example above, security group will have one inbound rule where the Source is `10.0.0.0/8` so that we allow traffic from all other regions.

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

For every region, Adjust the parameters under `managedNodeGroups` of the EKS yaml file as needed (e.g. `instanceType`, `desiredCapacity`, etc.). Remember to specify extra instances in the AWS region that holds the global resources.

The full schema of this config file can be found [here](https://eksctl.io/usage/schema/#managedNodeGroups).

### Specify AWS regions

Specify the regions and global region in the `deploy/regions.yaml` file. The regions information in this file will be the source of truth for the rest of the deployment process.

### Run the EKS clusters creation script

Specify the regions and global region that you want to deploy to in the `deploy/regions.yaml` file. Adjust the parameters in the yaml files in `deploy/eks` as needed, remember to specify extra instances in the AWS region that holds the global resources. 

Run the following command to create the EKS clusters:

```
python3 eks.py create
```

This process may take a few minutes to complete.

## Deploy the system

Adjust the parameters in the file `deploy/helm-neon/values.yaml` as needed (note that the `regions` field will be overwritten by the tool). Then run the following command to deploy the system:

```
python3 deploy.py 
```

This tool also provides options to skip some stages in the development process if needed. Run `python3 deploy.py -h` to see the available options.

## Run the benchmark

WIP