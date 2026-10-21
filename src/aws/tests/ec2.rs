//! Tests for `aws`: EC2.

use super::*;

#[tokio::test]
async fn list_subnets_in_vpc_filters_orders_and_extracts_name_tag() {
    use aws_sdk_ec2::operation::describe_subnets::DescribeSubnetsOutput;
    use aws_sdk_ec2::types::{Subnet, Tag};

    let ds = mock!(aws_sdk_ec2::Client::describe_subnets).then_output(|| {
        DescribeSubnetsOutput::builder()
            .subnets(
                Subnet::builder()
                    .subnet_id("subnet-2b")
                    .availability_zone("us-east-1b")
                    .cidr_block("10.0.2.0/24")
                    .tags(Tag::builder().key("Name").value("private-2b").build())
                    .build(),
            )
            .subnets(
                Subnet::builder()
                    .subnet_id("subnet-1a")
                    .availability_zone("us-east-1a")
                    .cidr_block("10.0.1.0/24")
                    .build(),
            )
            .subnets(
                Subnet::builder()
                    .subnet_id("subnet-1a-overlap")
                    .availability_zone("us-east-1a")
                    .cidr_block("10.0.0.0/24")
                    .build(),
            )
            .build()
    });
    let ec2 = mock_client!(aws_sdk_ec2, [&ds]);
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let client = AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        ec2,
    );

    let subnets = client.list_subnets_in_vpc("vpc-abc").await.expect("ok");
    // Ordered by AZ then CIDR — subnet-1a-overlap (10.0.0.0/24) precedes
    // subnet-1a (10.0.1.0/24), then subnet-2b.
    let ids: Vec<&str> = subnets.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["subnet-1a-overlap", "subnet-1a", "subnet-2b"]);
    // Name tag extracted when present, None when absent.
    assert_eq!(subnets[2].name_tag.as_deref(), Some("private-2b"));
    assert!(subnets[1].name_tag.is_none());
}

#[tokio::test]
async fn list_security_groups_in_vpc_orders_by_name() {
    use aws_sdk_ec2::operation::describe_security_groups::DescribeSecurityGroupsOutput;
    use aws_sdk_ec2::types::SecurityGroup;

    let dsg = mock!(aws_sdk_ec2::Client::describe_security_groups).then_output(|| {
        DescribeSecurityGroupsOutput::builder()
            .security_groups(
                SecurityGroup::builder()
                    .group_id("sg-z")
                    .group_name("zeta")
                    .description("z group")
                    .build(),
            )
            .security_groups(
                SecurityGroup::builder()
                    .group_id("sg-a")
                    .group_name("alpha")
                    .description("a group")
                    .build(),
            )
            .build()
    });
    let ec2 = mock_client!(aws_sdk_ec2, [&dsg]);
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let client = AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        ec2,
    );

    let sgs = client
        .list_security_groups_in_vpc("vpc-abc")
        .await
        .expect("ok");
    assert_eq!(sgs.len(), 2);
    assert_eq!(sgs[0].group_name, "alpha");
    assert_eq!(sgs[1].group_name, "zeta");
}

#[tokio::test]
async fn list_subnets_in_vpc_surfaces_ec2_errors_with_op_context() {
    use aws_sdk_ec2::operation::describe_subnets::DescribeSubnetsError;
    let rule = mock!(aws_sdk_ec2::Client::describe_subnets).then_error(|| {
        DescribeSubnetsError::generic(
            aws_smithy_types::error::ErrorMetadata::builder()
                .code("InvalidVpcID.NotFound")
                .message("vpc-xxx not found")
                .build(),
        )
    });
    let ec2 = mock_client!(aws_sdk_ec2, [&rule]);
    let cfg = aws_config::SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .behavior_version(aws_config::BehaviorVersion::latest())
        .build();
    let client = AwsClient::for_tests(
        Client::new(&cfg),
        SqsClient::new(&cfg),
        CwClient::new(&cfg),
        CwLogsClient::new(&cfg),
        S3Client::new(&cfg),
        ec2,
    );

    let err = client
        .list_subnets_in_vpc("vpc-xxx")
        .await
        .expect_err("expected EC2 error to propagate");
    assert!(
        err.to_string().contains("DescribeSubnets"),
        "expected operation context, got {err}"
    );
}

#[tokio::test]
async fn list_security_groups_in_vpc_follows_next_token() {
    // A shared VPC can hold more security groups than one page, and a
    // picker that silently shows a subset makes the operator conclude
    // the group doesn't exist and create a duplicate.
    use aws_sdk_ec2::operation::describe_security_groups::DescribeSecurityGroupsOutput;
    use aws_sdk_ec2::types::SecurityGroup;

    fn sg(id: &str, name: &str) -> SecurityGroup {
        SecurityGroup::builder()
            .group_id(id)
            .group_name(name)
            .description("d")
            .build()
    }
    let page1 = mock!(aws_sdk_ec2::Client::describe_security_groups)
        .match_requests(|req| req.next_token().is_none())
        .then_output(|| {
            DescribeSecurityGroupsOutput::builder()
                .security_groups(sg("sg-1", "alpha"))
                .next_token("P2")
                .build()
        });
    let page2 = mock!(aws_sdk_ec2::Client::describe_security_groups)
        .match_requests(|req| req.next_token() == Some("P2"))
        .then_output(|| {
            DescribeSecurityGroupsOutput::builder()
                .security_groups(sg("sg-2", "zulu"))
                .build()
        });
    let ec2 = mock_client!(aws_sdk_ec2, [&page1, &page2]);
    let client = client_with_ec2(ec2);

    let groups = client.list_security_groups_in_vpc("vpc-123").await.unwrap();
    let names: Vec<&str> = groups.iter().map(|g| g.group_name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "zulu"], "both pages must appear");
    assert_eq!(page2.num_calls(), 1, "next_token must be followed");
}

#[tokio::test]
async fn list_subnets_in_vpc_follows_next_token() {
    use aws_sdk_ec2::operation::describe_subnets::DescribeSubnetsOutput;
    use aws_sdk_ec2::types::Subnet;

    fn sn(id: &str, az: &str, cidr: &str) -> Subnet {
        Subnet::builder()
            .subnet_id(id)
            .availability_zone(az)
            .cidr_block(cidr)
            .build()
    }
    let page1 = mock!(aws_sdk_ec2::Client::describe_subnets)
        .match_requests(|req| req.next_token().is_none())
        .then_output(|| {
            DescribeSubnetsOutput::builder()
                .subnets(sn("subnet-1", "us-east-1a", "10.0.1.0/24"))
                .next_token("P2")
                .build()
        });
    let page2 = mock!(aws_sdk_ec2::Client::describe_subnets)
        .match_requests(|req| req.next_token() == Some("P2"))
        .then_output(|| {
            DescribeSubnetsOutput::builder()
                .subnets(sn("subnet-2", "us-east-1b", "10.0.2.0/24"))
                .build()
        });
    let ec2 = mock_client!(aws_sdk_ec2, [&page1, &page2]);
    let client = client_with_ec2(ec2);

    let subnets = client.list_subnets_in_vpc("vpc-123").await.unwrap();
    let ids: Vec<&str> = subnets.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["subnet-1", "subnet-2"]);
    assert_eq!(page2.num_calls(), 1, "next_token must be followed");
}
