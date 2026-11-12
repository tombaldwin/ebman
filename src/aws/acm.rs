//! ACM: listing issued certificates for the listener cert picker.

use super::*;

/// One ISSUED ACM certificate in the active region. Drives the
/// `:listener-edit` SSL-cert picker.
#[derive(Clone, Debug)]
pub(crate) struct AcmCert {
    pub arn: String,
    pub domain: String,
}

/// One ACM certificate a listener serves, as `DescribeCertificate`
/// reports it, with the days left worked out when it was read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CertStatus {
    pub arn: String,
    pub domain: String,
    pub not_after: Option<DateTime<Utc>>,
    /// Whole days until `not_after` when read; negative once expired.
    pub days_left: Option<i64>,
    /// `ISSUED`, `EXPIRED`, `REVOKED`, `PENDING_VALIDATION`, ...
    pub status: String,
    /// Managed renewal's state, when ACM is renewing it: `SUCCESS`,
    /// `PENDING_AUTO_RENEWAL`, `PENDING_VALIDATION`, `FAILED`.
    pub renewal: Option<String>,
}

impl CertStatus {
    /// A renewal ACM gave up on, or a certificate already past use.
    pub(crate) fn broken(&self) -> bool {
        self.renewal.as_deref() == Some("FAILED")
            || matches!(self.status.as_str(), "EXPIRED" | "REVOKED")
    }
}

/// The certificate ARNs an env's listeners serve, from its option
/// settings (`aws:elbv2:listener:PORT` `SSLCertificateArns`, comma
/// separated), each once. Pure.
pub(crate) fn listener_cert_arns(options: &[(String, String, String)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (ns, name, value) in options {
        if ns.starts_with("aws:elbv2:listener:") && name == "SSLCertificateArns" {
            for arn in value.split(',').map(str::trim).filter(|a| !a.is_empty()) {
                if !out.iter().any(|a| a == arn) {
                    out.push(arn.to_string());
                }
            }
        }
    }
    out
}

impl AwsClient {
    /// One certificate's expiry and renewal state, days left as of `now`.
    pub(crate) async fn describe_certificate(
        &self,
        arn: &str,
        now: DateTime<Utc>,
    ) -> Result<CertStatus> {
        let resp = self
            .acm()
            .describe_certificate()
            .certificate_arn(arn)
            .send()
            .await
            .aws_ctx("DescribeCertificate failed")?;
        let c = resp.certificate.ok_or_else(|| {
            color_eyre::eyre::eyre!("DescribeCertificate returned no certificate")
        })?;
        let not_after = c
            .not_after()
            .and_then(|d| DateTime::from_timestamp(d.secs(), d.subsec_nanos()));
        Ok(CertStatus {
            arn: arn.to_string(),
            domain: c.domain_name().unwrap_or_default().to_string(),
            days_left: not_after.map(|t| (t - now).num_days()),
            not_after,
            status: c
                .status()
                .map(|s| s.as_str().to_string())
                .unwrap_or_default(),
            renewal: c
                .renewal_summary()
                .map(|r| r.renewal_status().as_str().to_string()),
        })
    }

    /// List the region's ACM certificates (ISSUED only) as
    /// `(arn, primary domain)`. Drives the `:listener-edit` cert picker.
    pub(crate) async fn list_certificates(&self) -> Result<Vec<AcmCert>> {
        use aws_sdk_acm::types::CertificateStatus;
        let this = self;
        let raw = super::paginate("ListCertificates", move |token| async move {
            let mut req = this
                .acm()
                .list_certificates()
                .max_items(1000)
                .certificate_statuses(CertificateStatus::Issued);
            if let Some(t) = token {
                req = req.next_token(t);
            }
            let resp = req.send().await.aws_ctx("ListCertificates failed")?;
            Ok((
                resp.certificate_summary_list.unwrap_or_default(),
                resp.next_token,
            ))
        })
        .await?
        // Same reasoning as the VPC pickers: a truncated cert list in
        // `:listener-edit` reads as "that certificate isn't in ACM".
        .complete("ListCertificates")?;
        let mut out: Vec<AcmCert> = raw
            .into_iter()
            .filter_map(|c| {
                Some(AcmCert {
                    arn: c.certificate_arn?,
                    domain: c.domain_name.unwrap_or_default(),
                })
            })
            .collect();
        out.sort_by(|a, b| a.domain.cmp(&b.domain));
        Ok(out)
    }
}
