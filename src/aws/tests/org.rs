//! Tests for `aws`: Organizations.

use super::*;

#[tokio::test]
async fn list_org_accounts_sorts_active_first_then_by_name() {
    // The overlay puts ACTIVE accounts at the top so the operator
    // sees switchable accounts before suspended/closed ones. Pins
    // both the field mapping and the sort.
    use aws_sdk_organizations::operation::list_accounts::ListAccountsOutput;
    use aws_sdk_organizations::types::{Account, AccountStatus};

    let rule = mock!(aws_sdk_organizations::Client::list_accounts).then_output(|| {
        ListAccountsOutput::builder()
            .accounts(
                Account::builder()
                    .id("999999999999")
                    .name("zzz-closed")
                    .email("zzz@example.com")
                    .status(AccountStatus::Suspended)
                    .build(),
            )
            .accounts(
                Account::builder()
                    .id("222222222222")
                    .name("staging")
                    .email("staging@example.com")
                    .status(AccountStatus::Active)
                    .build(),
            )
            .accounts(
                Account::builder()
                    .id("111111111111")
                    .name("prod")
                    .email("prod@example.com")
                    .status(AccountStatus::Active)
                    .build(),
            )
            .build()
    });
    let org = mock_client!(aws_sdk_organizations, [&rule]);
    let client = client_with_sub!(org = org);

    let accounts = client.list_org_accounts().await.expect("ok");
    assert_eq!(accounts.len(), 3);
    // ACTIVE first, sorted by name within status.
    assert_eq!(accounts[0].name, "prod");
    assert_eq!(accounts[1].name, "staging");
    assert_eq!(accounts[2].name, "zzz-closed");
}

#[tokio::test]
async fn list_org_accounts_pages_past_the_default_runaway_guard() {
    // ListAccounts caps MaxResults at 20, so the shared 100-page
    // runaway guard put a hard 2,000-account ceiling on this walk —
    // and because the walk `.complete()`s, an org past that ceiling
    // got an error, not a short list. Real orgs run past 2,000
    // accounts. 150 pages here is comfortably over the old cap and
    // comfortably under SCAN_PAGES.
    use aws_sdk_organizations::operation::list_accounts::ListAccountsOutput;
    use aws_sdk_organizations::types::{Account, AccountStatus};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const PAGES: usize = 150;
    let seen = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = seen.clone();
    let rule = mock!(aws_sdk_organizations::Client::list_accounts).then_output(move || {
        let n = counter.fetch_add(1, Ordering::SeqCst);
        let mut b = ListAccountsOutput::builder().accounts(
            Account::builder()
                .id(format!("{:012}", n))
                .name(format!("acct-{n:04}"))
                .status(AccountStatus::Active)
                .build(),
        );
        if n + 1 < PAGES {
            b = b.next_token(format!("t{n}"));
        }
        b.build()
    });
    // MatchAny: one rule serves every page — the request shape only
    // differs by the token, and Sequential would exhaust after one.
    let org = mock_client!(
        aws_sdk_organizations,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&rule]
    );
    let client = client_with_sub!(org = org);

    let accounts = client
        .list_org_accounts()
        .await
        .expect("a large org must not error out of :accounts");
    assert_eq!(accounts.len(), PAGES);
    assert_eq!(seen.load(Ordering::SeqCst), PAGES);
}
