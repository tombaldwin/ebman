//! Tests for `aws`: ACM.

use super::*;

#[tokio::test]
async fn list_certificates_filters_to_issued_and_extracts_domain() {
    // Pins two contract points: (1) ListCertificates is called with
    // `CertificateStatus::Issued` so revoked / pending / expired
    // certs don't show up in the `:listener-edit` picker, and (2)
    // the response's `domain_name` lands in `AcmCert.domain`.
    use aws_sdk_acm::operation::list_certificates::ListCertificatesOutput;
    use aws_sdk_acm::types::{CertificateStatus, CertificateSummary};

    let rule = mock!(aws_sdk_acm::Client::list_certificates)
        .match_requests(|req| {
            req.certificate_statuses()
                .contains(&CertificateStatus::Issued)
        })
        .then_output(|| {
            ListCertificatesOutput::builder()
                .certificate_summary_list(
                    CertificateSummary::builder()
                        .certificate_arn("arn:aws:acm:us-east-1:123:certificate/abcd")
                        .domain_name("*.example.com")
                        .build(),
                )
                .certificate_summary_list(
                    CertificateSummary::builder()
                        .certificate_arn("arn:aws:acm:us-east-1:123:certificate/efgh")
                        .domain_name("api.example.com")
                        .build(),
                )
                .build()
        });
    let acm = mock_client!(aws_sdk_acm, [&rule]);
    let client = client_with_sub!(acm = acm);

    let certs = client.list_certificates().await.expect("ok");
    assert_eq!(certs.len(), 2);
    // Sorted by domain.
    assert_eq!(certs[0].domain, "*.example.com");
    assert_eq!(certs[1].domain, "api.example.com");
    assert_eq!(rule.num_calls(), 1, "ListCertificates fired once");
}
