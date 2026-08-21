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
        let mut out: Vec<AcmCert> = Vec::new();
        let mut next_token: Option<String> = None;
        loop {
            let mut req = self
                .acm
                .list_certificates()
                .certificate_statuses(CertificateStatus::Issued);
            if let Some(t) = next_token.take() {
                req = req.next_token(t);
            }
            let resp = req.send().await.wrap_err("ListCertificates failed")?;
            for c in resp.certificate_summary_list.unwrap_or_default() {
                if let Some(arn) = c.certificate_arn {
                    out.push(AcmCert {
                        arn,
                        domain: c.domain_name.unwrap_or_default(),
                    });
                }
            }
            match resp.next_token {
                Some(t) if !t.is_empty() => next_token = Some(t),
                _ => break,
            }
        }
        out.sort_by(|a, b| a.domain.cmp(&b.domain));
        Ok(out)
    }
}
