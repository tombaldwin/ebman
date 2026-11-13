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

/// EBL024's probe: each listener certificate described once, days left
/// worked out from `now`; a certificate that cannot be read is an error
/// the caller reports, never a valid certificate; switched off, or with
/// no HTTPS listener, nothing is asked.
#[tokio::test]
async fn the_listener_cert_probe_reads_each_certificate_once() {
    use aws_sdk_acm::operation::describe_certificate::{
        DescribeCertificateError, DescribeCertificateOutput,
    };
    use aws_sdk_acm::types::{CertificateDetail, CertificateStatus, RenewalStatus, RenewalSummary};
    // Whole seconds, as ACM's dates are: a fraction would round 12 days
    // down to 11.
    let now = chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp(), 0).expect("now");
    let in_days = move |d: i64| {
        aws_smithy_types::DateTime::from_secs((now + chrono::Duration::days(d)).timestamp())
    };
    let soon = mock!(aws_sdk_acm::Client::describe_certificate)
        .match_requests(|r| {
            r.certificate_arn() == Some("arn:aws:acm:us-east-1:123456789012:certificate/soon")
        })
        .then_output(move || {
            DescribeCertificateOutput::builder()
                .certificate(
                    CertificateDetail::builder()
                        .domain_name("api.example.com")
                        .status(CertificateStatus::Issued)
                        .not_after(in_days(12))
                        .renewal_summary(
                            RenewalSummary::builder()
                                .renewal_status(RenewalStatus::Failed)
                                .updated_at(in_days(0))
                                .set_domain_validation_options(Some(Vec::new()))
                                .build()
                                .unwrap(),
                        )
                        .build(),
                )
                .build()
        });
    let denied = mock!(aws_sdk_acm::Client::describe_certificate)
        .match_requests(|r| {
            r.certificate_arn() == Some("arn:aws:acm:us-east-1:123456789012:certificate/denied")
        })
        .then_error(|| {
            DescribeCertificateError::generic(
                aws_smithy_types::error::ErrorMetadata::builder()
                    .code("AccessDeniedException")
                    .message("CERTDENIED")
                    .build(),
            )
        });
    let acm = mock_client!(
        aws_sdk_acm,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&soon, &denied]
    );
    let client = client_with_sub!(acm = acm);
    let opts = |arns: &str| {
        vec![(
            "aws:elbv2:listener:443".to_string(),
            "SSLCertificateArns".to_string(),
            arns.to_string(),
        )]
    };
    let probe = crate::lint::inputs::probe_listener_certs;
    let certs = probe(&client, &opts("arn:aws:acm:us-east-1:123456789012:certificate/soon, arn:aws:acm:us-east-1:123456789012:certificate/soon"), &[], now)
        .await
        .expect("read")
        .expect("an HTTPS listener");
    assert_eq!(certs.len(), 1, "each certificate once");
    assert_eq!(soon.num_calls(), 1);
    let c = &certs[0];
    assert_eq!(c.domain, "api.example.com");
    assert_eq!(c.days_left, Some(12));
    assert_eq!(c.renewal.as_deref(), Some("FAILED"));
    assert!(c.broken(), "a failed renewal");
    // Read an hour after it expired: -1 days, not "expires in 0 days".
    let later = now + chrono::Duration::days(12) + chrono::Duration::hours(1);
    let expired = probe(
        &client,
        &opts("arn:aws:acm:us-east-1:123456789012:certificate/soon"),
        &[],
        later,
    )
    .await
    .expect("read")
    .expect("an HTTPS listener");
    assert_eq!(expired[0].days_left, Some(-1));

    let err = probe(
        &client,
        &opts("arn:aws:acm:us-east-1:123456789012:certificate/denied"),
        &[],
        now,
    )
    .await
    .expect_err("unread");
    assert!(err.contains("CERTDENIED"), "{err}");
    assert_eq!(
        probe(
            &client,
            &opts("arn:aws:acm:us-east-1:123456789012:certificate/soon"),
            &["EBL024".into()],
            now
        )
        .await,
        Ok(None)
    );
    assert_eq!(probe(&client, &[], &[], now).await, Ok(None));
    assert_eq!(
        soon.num_calls(),
        2,
        "nothing asked when off or without HTTPS"
    );
}

#[test]
fn only_acm_certificates_are_listed_in_any_partition() {
    use super::listener_cert_arns;
    let opt = |v: &str| {
        (
            "aws:elbv2:listener:443".to_string(),
            "SSLCertificateArns".to_string(),
            v.to_string(),
        )
    };
    let arns = listener_cert_arns(&[opt(
        "arn:aws:iam::123456789012:server-certificate/legacy, arn:aws:acm:us-east-1:123456789012:certificate/a, arn:aws-cn:acm:cn-north-1:123456789012:certificate/b, arn:aws:acmx:us-east-1:1:certificate/c",
    )]);
    assert_eq!(
        arns,
        [
            "arn:aws:acm:us-east-1:123456789012:certificate/a",
            "arn:aws-cn:acm:cn-north-1:123456789012:certificate/b",
        ]
    );
    let opts = [opt(
        "arn:aws:iam::123456789012:server-certificate/legacy, arn:aws:acm:us-east-1:123456789012:certificate/a",
    )];
    assert_eq!(
        super::listener_unchecked_cert_arns(&opts),
        ["arn:aws:iam::123456789012:server-certificate/legacy"]
    );
}

#[test]
fn days_left_round_down_so_an_expired_certificate_is_negative() {
    use crate::aws::acm::days_until;
    let now = chrono::DateTime::parse_from_rfc3339("2026-11-12T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let at = |h: i64| now + chrono::Duration::hours(h);
    assert_eq!(days_until(at(-1), now), -1, "expired an hour ago");
    assert_eq!(days_until(at(1), now), 0);
    assert_eq!(days_until(at(24), now), 1);
    assert_eq!(days_until(at(47), now), 1);
    assert_eq!(days_until(at(-25), now), -2);
}
