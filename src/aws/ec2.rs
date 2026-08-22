//! EC2: instance termination, and the VPC context (subnets, security
//! groups) the environment forms need to offer real choices.

use super::*;

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
        // Paginate: DescribeSubnets caps a page well below the size of a
        // shared VPC's subnet list, and a picker that silently shows a
        // subset is worse than a slow one — the operator concludes the
        // subnet doesn't exist.
        let (this, vpc) = (self, vpc_id);
        let raw = super::paginate("DescribeSubnets", move |token| async move {
            let mut req = this.ec2.describe_subnets().max_results(1000).filters(
                Filter::builder()
                    .name("vpc-id")
                    .values(vpc.to_string())
                    .build(),
            );
            if let Some(t) = token {
                req = req.next_token(t);
            }
            let resp = req.send().await.wrap_err("DescribeSubnets failed")?;
            Ok((resp.subnets.unwrap_or_default(), resp.next_token))
        })
        .await?
        // `.complete()`, not `.items()`: this feeds a picker, and a
        // picker showing a prefix of the real list doesn't read as
        // "short" — the operator scrolls, doesn't find what they want,
        // and concludes it isn't there.
        .complete("DescribeSubnets")?;
        let mut out: Vec<SubnetInfo> = raw
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
        // Paginate: DescribeSecurityGroups defaults to 1000 per page and
        // a shared VPC can exceed that. Same reasoning as the subnet
        // listing above — a truncated picker reads as "not there".
        let (this, vpc) = (self, vpc_id);
        let raw = super::paginate("DescribeSecurityGroups", move |token| async move {
            let mut req = this
                .ec2
                .describe_security_groups()
                .max_results(1000)
                .filters(
                    Filter::builder()
                        .name("vpc-id")
                        .values(vpc.to_string())
                        .build(),
                );
            if let Some(t) = token {
                req = req.next_token(t);
            }
            let resp = req.send().await.wrap_err("DescribeSecurityGroups failed")?;
            Ok((resp.security_groups.unwrap_or_default(), resp.next_token))
        })
        .await?
        .complete("DescribeSecurityGroups")?;
        let mut out: Vec<SecurityGroupInfo> = raw
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
