# EKS Deployment Instructions

## One-time network configuration

### Set up VPC peering

For pods to communicate across separate Kubernetes clusters, the VPCs in all regions need to be peered. Network traffic can then be routed between the VPCs. For more information about VPC peering, see the [AWS documentation](https://docs.aws.amazon.com/vpc/latest/peering/what-is-vpc-peering.html).

1. Open the [Amazon VPC console](https://console.aws.amazon.com/vpc/) and create a new VPC for each region. Specify unique and **non-overlapping** IP ranges for the VPCs (e.g., 10.0.0.0/16).
2. Note the ID of the VPC in each region. The VPC ID is found in the section called Your VPCs.
3. Navigate to the Peering Connections section and [create a VPC peering connection](https://docs.aws.amazon.com/vpc/latest/peering/create-vpc-peering-connection.html#create-vpc-peering-connection-local) between each of the regions. When you create a peering connection, you will first select a requester VPC in the current region and then an accepter VPC in another region, specified by pasting the VPC ID.   
4. To complete the VPC peering connections, switch to each destination region and [accept the pending connection](https://docs.aws.amazon.com/vpc/latest/peering/create-vpc-peering-connection.html#accept-vpc-peering-connection) in the VPC console.
5. For all regions, navigate to the Route Tables section and find `PublicRouteTable`. [Update this route table](https://docs.aws.amazon.com/vpc/latest/peering/vpc-peering-routing.html) with new entries that point traffic to the other peered VPCs. The **Destination** should be the CIDR block of a destination region, and the **Target** should be the VPC peering connection between the current and the destination region.

### Create subnets

Create at least one subnet in each VPC and fill the subnet IDs in the "vpc.subnets" field of the EKS yaml file (e.g. `deploy/eks/us-east-1.yml`) of the corresponding region.

### Create security groups

For each region, create the a security group that allows all traffic from other regions. Assuming the VPC in every region uses a CIDR block with the form `10.X.X.X/16`, the security group should have the one inbound rule where the Source is `10.0.0.0/8`.

Copy the ID of this security group and use it in the `securityGroups` array in the EKS yaml file (e.g. `deploy/eks/us-east-1.yml`) of the corresponding region.

## Create EKS clusters

Specify the regions and global region that you want to deploy to in the `deploy/eks/regions.yaml` file. Adjust the parameters in the yaml files in `deploy/eks` as needed, remember to specify extra instances in the AWS region that holds the global resources. 

Run the following command to create the EKS clusters:

```
python3 eks.py create
```

This process may take a few minutes to complete.

## Deploy the system

Adjust the parameters in the file `helm-neon/values.yaml` as needed. Then run the following command to deploy the system:

```
python3 deploy.py 
```

This tool also provides options to skip some stages in the development process if needed. Run `python3 deploy.py -h` to see the available options.

## Run the benchmark

WIP