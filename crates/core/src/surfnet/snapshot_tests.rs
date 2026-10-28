use std::{collections::BTreeMap, str::FromStr};

use async_trait::async_trait;
use solana_client::{
    nonblocking::rpc_client::RpcClient, rpc_client::RpcClientConfig, rpc_request::RpcRequest,
};
use solana_rpc_client::rpc_sender::{RpcSender, RpcTransportStats};
use solana_rpc_client_api::client_error::{ErrorKind as ClientErrorKind, Result as ClientResult};

use super::*;

struct SnapshotSender {
    accounts: BTreeMap<String, AccountSnapshot>,
    fail: bool,
}

#[async_trait]
impl RpcSender for SnapshotSender {
    async fn send(
        &self,
        request: RpcRequest,
        params: serde_json::Value,
    ) -> ClientResult<serde_json::Value> {
        if self.fail {
            return Err(ClientErrorKind::Custom("provider unavailable".into()).into());
        }
        assert_eq!(request, RpcRequest::GetMultipleAccounts);
        let values: Vec<_> = params[0]
            .as_array()
            .unwrap()
            .iter()
            .map(|key| {
                self.accounts.get(key.as_str().unwrap()).map(|a| {
                    serde_json::json!({
                        "lamports": a.lamports, "owner": a.owner, "executable": a.executable,
                        "rentEpoch": a.rent_epoch, "data": [a.data, "base64"],
                    })
                })
            })
            .collect();
        Ok(serde_json::json!({"context": {"slot": 1}, "value": values}))
    }
    fn get_transport_stats(&self) -> RpcTransportStats {
        RpcTransportStats::default()
    }
    fn url(&self) -> String {
        "http://snapshot.example".into()
    }
}

fn remote(accounts: BTreeMap<String, AccountSnapshot>, fail: bool) -> SurfnetRemoteClient {
    SurfnetRemoteClient {
        client: RpcClient::new_sender(
            SnapshotSender { accounts, fail },
            RpcClientConfig::default(),
        )
        .into(),
    }
}

fn locker() -> SurfnetSvmLocker {
    let (svm, _, _) = SurfnetSvm::default();
    SurfnetSvmLocker::new(svm)
}

fn account() -> AccountSnapshot {
    AccountSnapshot {
        lamports: 1_000_000,
        owner: Pubkey::new_unique().to_string(),
        executable: false,
        rent_epoch: 0,
        data: "AQID".into(),
        parsed_data: None,
    }
}

fn live(locker: &SurfnetSvmLocker, key: &str) -> Option<Account> {
    let key = Pubkey::from_str(key).unwrap();
    locker.with_svm_reader(|svm| svm.inner.get_account_no_db(&key))
}

fn fixture() -> BTreeMap<String, Option<AccountSnapshot>> {
    serde_json::from_str(include_str!("../tests/assets/program_snapshot.json")).unwrap()
}

fn fixture_keys(snapshot: &BTreeMap<String, Option<AccountSnapshot>>) -> (String, String) {
    let program = snapshot
        .iter()
        .find(|(_, a)| a.as_ref().unwrap().executable)
        .unwrap()
        .0
        .clone();
    let data = snapshot
        .iter()
        .find(|(_, a)| !a.as_ref().unwrap().executable)
        .unwrap()
        .0
        .clone();
    (program, data)
}

#[tokio::test(flavor = "multi_thread")]
async fn strict_installs_all_explicit_accounts_in_live_svm() {
    let locker = locker();
    let first = Pubkey::new_unique().to_string();
    let second = Pubkey::new_unique().to_string();
    let snapshot = BTreeMap::from([
        (first.clone(), Some(account())),
        (second.clone(), Some(account())),
    ]);
    assert_eq!(
        locker
            .load_snapshot_with_options(&snapshot, None, CommitmentConfig::confirmed(), true)
            .await
            .unwrap(),
        2
    );
    assert_eq!(live(&locker, &first).unwrap().data, vec![1, 2, 3]);
    assert_eq!(live(&locker, &second).unwrap().data, vec![1, 2, 3]);
}

#[tokio::test(flavor = "multi_thread")]
async fn strict_hydrates_complete_remote_map_and_rejects_missing_and_rpc_failure() {
    let first = Pubkey::new_unique().to_string();
    let second = Pubkey::new_unique().to_string();
    let snapshot = BTreeMap::from([(first.clone(), None), (second.clone(), None)]);
    let values = BTreeMap::from([(first.clone(), account()), (second.clone(), account())]);
    let initial_locker = locker();
    assert_eq!(
        initial_locker
            .load_snapshot_with_options(
                &snapshot,
                Some(&remote(values.clone(), false)),
                CommitmentConfig::confirmed(),
                true
            )
            .await
            .unwrap(),
        2
    );
    assert!(live(&initial_locker, &first).is_some() && live(&initial_locker, &second).is_some());
    let missing = remote(
        BTreeMap::from([(first.clone(), values[&first].clone())]),
        false,
    );
    let error = locker()
        .load_snapshot_with_options(
            &snapshot,
            Some(&missing),
            CommitmentConfig::confirmed(),
            true,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&second) && error.to_lowercase().contains("absent"),
        "{error}"
    );
    let error = locker()
        .load_snapshot_with_options(
            &snapshot,
            Some(&remote(BTreeMap::new(), true)),
            CommitmentConfig::confirmed(),
            true,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&first) && error.contains("upstream fetch failed"),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn strict_rejects_null_without_remote_and_invalid_inputs() {
    let key = Pubkey::new_unique().to_string();
    let missing = BTreeMap::from([(key.clone(), None)]);
    let error = locker()
        .load_snapshot_with_options(&missing, None, CommitmentConfig::confirmed(), true)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&key) && error.to_lowercase().contains("upstream"),
        "{error}"
    );
    let mut invalid_data = account();
    invalid_data.data = "%%%".into();
    let mut invalid_owner = account();
    invalid_owner.owner = "bad-owner".into();
    let data_key = Pubkey::new_unique().to_string();
    let owner_key = Pubkey::new_unique().to_string();
    let snapshot = BTreeMap::from([
        ("invalid-key".into(), Some(account())),
        (data_key.clone(), Some(invalid_data)),
        (owner_key.clone(), Some(invalid_owner)),
    ]);
    let error = locker()
        .load_snapshot_with_options(&snapshot, None, CommitmentConfig::confirmed(), true)
        .await
        .unwrap_err()
        .to_string();
    for part in ["invalid-key", &data_key, &owner_key, "base64", "owner"] {
        assert!(error.contains(part), "missing {part}: {error}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn strict_rejects_invalid_executable_installation() {
    let key = Pubkey::new_unique().to_string();
    let mut program = account();
    program.owner = solana_sdk_ids::bpf_loader::id().to_string();
    program.executable = true;
    let snapshot = BTreeMap::from([(key.clone(), Some(program))]);
    let error = locker()
        .load_snapshot_with_options(&snapshot, None, CommitmentConfig::confirmed(), true)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&key) && error.contains("failed to set account"),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn strict_requires_local_upgradeable_programdata() {
    let snapshot = fixture();
    let (program, data) = fixture_keys(&snapshot);
    let missing = BTreeMap::from([(program.clone(), snapshot[&program].clone())]);
    let error = locker()
        .load_snapshot_with_options(&missing, None, CommitmentConfig::confirmed(), true)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains(&program) && error.contains(&data), "{error}");
    let locker = locker();
    assert_eq!(
        locker
            .load_snapshot_with_options(&snapshot, None, CommitmentConfig::confirmed(), true)
            .await
            .unwrap(),
        2
    );
    assert!(live(&locker, &program).is_some() && live(&locker, &data).is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn strict_requires_remote_upgradeable_programdata() {
    let fixture = fixture();
    let (program, data) = fixture_keys(&fixture);
    let snapshot = BTreeMap::from([(program.clone(), None)]);
    let values: BTreeMap<_, _> = fixture.into_iter().map(|(k, v)| (k, v.unwrap())).collect();
    let missing = remote(
        BTreeMap::from([(program.clone(), values[&program].clone())]),
        false,
    );
    let error = locker()
        .load_snapshot_with_options(
            &snapshot,
            Some(&missing),
            CommitmentConfig::confirmed(),
            true,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains(&program) && error.contains(&data), "{error}");
    let locker = locker();
    locker
        .load_snapshot_with_options(
            &snapshot,
            Some(&remote(values, false)),
            CommitmentConfig::confirmed(),
            true,
        )
        .await
        .unwrap();
    assert!(live(&locker, &program).is_some() && live(&locker, &data).is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn default_snapshot_loader_remains_best_effort() {
    let key = Pubkey::new_unique().to_string();
    let absent = Pubkey::new_unique().to_string();
    let snapshot = BTreeMap::from([
        (key.clone(), Some(account())),
        (absent.clone(), None),
        ("bad-key".into(), Some(account())),
    ]);
    let locker = locker();
    assert_eq!(
        locker
            .load_snapshot(&snapshot, None, CommitmentConfig::confirmed())
            .await
            .unwrap(),
        1
    );
    assert!(live(&locker, &key).is_some() && live(&locker, &absent).is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn strict_does_not_require_guessed_programdata_for_legacy_executable() {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let fixture = fixture();
    let (_, data_key) = fixture_keys(&fixture);
    let programdata = fixture[&data_key].as_ref().unwrap();
    let data = STANDARD.decode(&programdata.data).unwrap();
    let key = Pubkey::new_unique().to_string();
    let program = AccountSnapshot {
        lamports: 20_580_720,
        owner: solana_sdk_ids::bpf_loader::id().to_string(),
        executable: true,
        rent_epoch: 0,
        data: STANDARD.encode(&data[UpgradeableLoaderState::size_of_programdata_metadata()..]),
        parsed_data: None,
    };
    let snapshot = BTreeMap::from([(key.clone(), None)]);
    let locker = locker();
    locker
        .load_snapshot_with_options(
            &snapshot,
            Some(&remote(BTreeMap::from([(key.clone(), program)]), false)),
            CommitmentConfig::confirmed(),
            true,
        )
        .await
        .unwrap();
    assert!(live(&locker, &key).is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn strict_reports_remote_executable_installation_failure() {
    let key = Pubkey::new_unique().to_string();
    let mut program = account();
    program.owner = solana_sdk_ids::bpf_loader::id().to_string();
    program.executable = true;
    let snapshot = BTreeMap::from([(key.clone(), None)]);
    let error = locker()
        .load_snapshot_with_options(
            &snapshot,
            Some(&remote(BTreeMap::from([(key.clone(), program)]), false)),
            CommitmentConfig::confirmed(),
            true,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&key) && error.contains("failed to set account"),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn strict_rejects_malformed_live_programdata() {
    let mut snapshot = fixture();
    let (program, data) = fixture_keys(&snapshot);
    snapshot.insert(data.clone(), Some(account()));
    let error = locker()
        .load_snapshot_with_options(&snapshot, None, CommitmentConfig::confirmed(), true)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&program) && error.contains(&data) && error.contains("invalid ProgramData"),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn default_snapshot_loader_tolerates_missing_and_failing_remote() {
    let key = Pubkey::new_unique().to_string();
    let snapshot = BTreeMap::from([(key.clone(), None)]);
    for fail in [false, true] {
        let locker = locker();
        assert_eq!(
            locker
                .load_snapshot(
                    &snapshot,
                    Some(&remote(BTreeMap::new(), fail)),
                    CommitmentConfig::confirmed()
                )
                .await
                .unwrap(),
            0
        );
        assert!(live(&locker, &key).is_none());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn strict_rejects_remote_executable_shadowed_by_live_non_executable() {
    let fixture = fixture();
    let (program, data) = fixture_keys(&fixture);
    let local = account();
    let locker = locker();
    locker
        .load_snapshot(
            &BTreeMap::from([(program.clone(), Some(local.clone()))]),
            None,
            CommitmentConfig::confirmed(),
        )
        .await
        .unwrap();
    let values = fixture
        .into_iter()
        .map(|(key, value)| (key, value.unwrap()))
        .collect();
    let error = locker
        .load_snapshot_with_options(
            &BTreeMap::from([(program.clone(), None)]),
            Some(&remote(values, false)),
            CommitmentConfig::confirmed(),
            true,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&program) && error.contains("requested executable is not executable"),
        "{error}"
    );
    let live_program = live(&locker, &program).unwrap();
    assert!(!live_program.executable);
    assert_eq!(live_program.data, vec![1, 2, 3]);
    assert!(live(&locker, &data).is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn strict_requires_explicit_mint_but_not_implicitly_fetched_mint() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use solana_program_pack::Pack;
    use spl_token_interface::state::{Account as TokenAccount, AccountState};

    let mint = Pubkey::new_unique();
    let token = Pubkey::new_unique().to_string();
    let mut data = vec![0; TokenAccount::LEN];
    TokenAccount {
        mint,
        owner: Pubkey::new_unique(),
        state: AccountState::Initialized,
        ..Default::default()
    }
    .pack_into_slice(&mut data);
    let mut token_account = account();
    token_account.owner = spl_token_interface::id().to_string();
    token_account.data = STANDARD.encode(data);
    let values = BTreeMap::from([(token.clone(), token_account)]);
    let snapshot = BTreeMap::from([(token.clone(), None)]);
    let first_locker = locker();
    first_locker
        .load_snapshot_with_options(
            &snapshot,
            Some(&remote(values.clone(), false)),
            CommitmentConfig::confirmed(),
            true,
        )
        .await
        .unwrap();
    assert!(live(&first_locker, &token).is_some());
    assert!(live(&first_locker, &mint.to_string()).is_none());
    let explicit = BTreeMap::from([(token.clone(), None), (mint.to_string(), None)]);
    let error = locker()
        .load_snapshot_with_options(
            &explicit,
            Some(&remote(values, false)),
            CommitmentConfig::confirmed(),
            true,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&mint.to_string()) && error.contains("absent from upstream"),
        "{error}"
    );
}
