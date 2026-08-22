//! ACM: listing issued certificates for the listener cert picker.

use super::*;

/// One ISSUED ACM certificate in the active region. Drives the
/// `:listener-edit` SSL-cert picker.
#[derive(Clone, Debug)]
pub struct AcmCert {
    pub arn: String,
    pub domain: String,
}

impl AwsClient {
    /// List the region's ACM certificates (ISSUED only) as
    /// `(arn, primary domain)`. Drives the `:listener-edit` cert picker.
    pub async fn list_certificates(&self) -> Result<Vec<AcmCert>> {
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
            let resp = req.send().await.wrap_err("ListCertificates failed")?;
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
