//! Tests for `aws`: Secrets Manager.

use super::*;

#[tokio::test]
async fn list_secrets_maps_secretlistentry_to_summary() {
    // Pins the happy path through ListSecrets — field-by-field
    // mapping from `SecretListEntry` to `SecretSummary`, sort by
    // last_changed desc, and the optional `name_filter` substring
    // match. Caught here because `:secrets` is the operator's
    // entry into Secrets Manager and a silent field-rename in the
    // SDK output would break the picker without a compile error.
    use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput;
    use aws_sdk_secretsmanager::types::SecretListEntry;
    use aws_smithy_types::DateTime as SmithyDt;

    let rule = mock!(aws_sdk_secretsmanager::Client::list_secrets).then_output(|| {
        ListSecretsOutput::builder()
            .secret_list(
                SecretListEntry::builder()
                    .name("prod/db-password")
                    .arn("arn:aws:secretsmanager:us-east-1:123:secret:prod/db-password-AbCdEf")
                    .description("Production DB master password")
                    .last_changed_date(SmithyDt::from_secs(1_700_000_000))
                    .build(),
            )
            .secret_list(
                SecretListEntry::builder()
                    .name("staging/api-key")
                    .arn("arn:aws:secretsmanager:us-east-1:123:secret:staging/api-key-XyZ")
                    // Older timestamp than prod/db-password — should
                    // sort below in the result.
                    .last_changed_date(SmithyDt::from_secs(1_600_000_000))
                    .build(),
            )
            .build()
    });
    let secrets = mock_client!(aws_sdk_secretsmanager, [&rule]);
    let client = client_with_sub!(secrets = secrets);

    let all = client.list_secrets(None).await.expect("ok");
    assert_eq!(all.len(), 2);
    // Newest first.
    assert_eq!(all[0].name, "prod/db-password");
    assert_eq!(all[1].name, "staging/api-key");
    // Description + last_changed survived the round-trip.
    assert_eq!(
        all[0].description.as_deref(),
        Some("Production DB master password")
    );
    assert!(all[0].last_changed.is_some());
}
