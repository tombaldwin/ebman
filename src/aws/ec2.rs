//! EC2: instance termination, and the VPC context (subnets, security
//! groups) the environment forms need to offer real choices.

use super::*;

/// Result of `fetch_env_vpc_context` — the env's VPC plus the option-
/// settings selections the `:subnets` / `:elb-subnets` / `:security-groups`
/// pickers need for their pre-fill. Each field is `None` / empty when the
/// env doesn't override that option (EB uses its account-default in that
/// case).
#[derive(Clone, Debug, Default)]
pub struct EnvVpcContext {
    pub vpc_id: Option<String>,
    pub subnets: Vec<String>,
    /// ELB subnets (`aws:ec2:vpc.ELBSubnets`). Web-tier envs typically
    /// attach the ELB to a separate subnet set than the instance subnets;
    /// worker envs leave this empty.
    pub elb_subnets: Vec<String>,
    pub security_groups: Vec<String>,
}

/// One subnet in a VPC. Used by `:subnets` to populate the picker.
#[derive(Clone, Debug)]
pub struct SubnetInfo {
    pub id: String,
    pub availability_zone: String,
    pub cidr_block: String,
    /// Friendly name from the `Name` tag, if any.
    pub name_tag: Option<String>,
}

/// One security group in a VPC. Used by `:security-groups`.
#[derive(Clone, Debug)]
pub struct SecurityGroupInfo {
    pub id: String,
    pub group_name: String,
    pub description: String,
}

impl AwsClient {
    /// List subnets in a VPC, ordered by AZ then CIDR for stable picker
    /// rows. Returns the wide rows the `:subnets` picker needs (id + AZ
    /// + CIDR + Name tag) so callers don't need a second round-trip.
    pub async fn list_subnets_in_vpc(&self, vpc_id: &str) -> Result<Vec<SubnetInfo>> {
        use aws_sdk_ec2::types::Filter;
        let resp = self
            .ec2
            .describe_subnets()
            .filters(
                Filter::builder()
                    .name("vpc-id")
                    .values(vpc_id.to_string())
                    .build(),
            )
            .send()
            .await
            .wrap_err("DescribeSubnets failed")?;
        let mut out: Vec<SubnetInfo> = resp
            .subnets
            .unwrap_or_default()
            .into_iter()
            .map(|s| {
                let name_tag = s.tags.as_ref().and_then(|tags| {
                    tags.iter()
                        .find(|t| t.key.as_deref() == Some("Name"))
                        .and_then(|t| t.value.clone())
                });
                SubnetInfo {
                    id: s.subnet_id.unwrap_or_default(),
                    availability_zone: s.availability_zone.unwrap_or_default(),
                    cidr_block: s.cidr_block.unwrap_or_default(),
                    name_tag,
                }
            })
            .collect();
        out.sort_by(|a, b| {
            a.availability_zone
                .cmp(&b.availability_zone)
                .then(a.cidr_block.cmp(&b.cidr_block))
        });
        Ok(out)
    }

    /// List security groups in a VPC, ordered by name for stable picker
    /// rows.
    pub async fn list_security_groups_in_vpc(
        &self,
        vpc_id: &str,
    ) -> Result<Vec<SecurityGroupInfo>> {
        use aws_sdk_ec2::types::Filter;
        let resp = self
            .ec2
            .describe_security_groups()
            .filters(
                Filter::builder()
                    .name("vpc-id")
                    .values(vpc_id.to_string())
                    .build(),
            )
            .send()
            .await
            .wrap_err("DescribeSecurityGroups failed")?;
        let mut out: Vec<SecurityGroupInfo> = resp
            .security_groups
            .unwrap_or_default()
            .into_iter()
            .map(|g| SecurityGroupInfo {
                id: g.group_id.unwrap_or_default(),
                group_name: g.group_name.unwrap_or_default(),
                description: g.description.unwrap_or_default(),
            })
            .collect();
        out.sort_by(|a, b| a.group_name.cmp(&b.group_name));
        Ok(out)
    }

    /// Terminate a single EC2 instance by ID. ASG (created by EB) re-launches
    /// a replacement automatically. The API returns immediately; the
    /// instance enters `shutting-down` and EB's events panel will surface
    /// the replacement within ~30 s.
    pub async fn terminate_instance(&self, instance_id: &str) -> Result<()> {
        self.ec2
            .terminate_instances()
            .instance_ids(instance_id)
            .send()
            .await
            .wrap_err("ec2:TerminateInstances failed")?;
        Ok(())
    }
}
