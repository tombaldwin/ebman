//! WAFv2: resolving the web ACL associated with an environment's
//! load balancer.

use super::*;

impl AwsClient {
    /// WAFv2 `GetWebACLForResource` for a REGIONAL resource (an env's
    /// ALB). Returns the associated WebACL's ARN, or `None` when
    /// nothing is attached. The wafv2 client is built lazily from the
    /// stored `SdkConfig` — this is a rare, lint-probe-only call
    /// (EBL018), not worth a permanent sub-client field.
    pub(crate) async fn web_acl_for_resource(&self, resource_arn: &str) -> Result<Option<String>> {
        let waf = aws_sdk_wafv2::Client::new(&self.config);
        let resp = waf
            .get_web_acl_for_resource()
            .resource_arn(resource_arn)
            .send()
            .await
            .wrap_err("GetWebACLForResource failed")?;
        Ok(resp.web_acl.map(|a| a.arn))
    }
}
