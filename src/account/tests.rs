use super::model::*;
use super::store::*;
use super::*;
use crate::dpapi;

#[test]
fn connected_status_prefers_trimmed_nickname_and_derives_avatar_url() {
    let credentials = Credentials {
        access_token: "token".to_string(),
        username: Some("cnb.user/name".to_string()),
        nickname: Some("  九哥  ".to_string()),
        auth_method: AuthMethod::PersonalAccessToken,
        token_id: Some("token-id".to_string()),
    };

    let status = AccountStatus::connected(&credentials);

    assert_eq!(status.display_name.as_deref(), Some("九哥"));
    assert_eq!(
        status.avatar_url.as_deref(),
        Some("https://cnb.cool/users/cnb.user%2Fname/avatar/l")
    );
}

#[test]
fn saved_account_exposes_the_trimmed_nickname_for_display() {
    let dir = tempfile::tempdir().unwrap();
    let manager =
        AccountManager::with_store(AccountStore::new(dir.path().join("account-auth.bin")));
    manager
        .complete_token_login(
            "token",
            Some("alice".to_string()),
            Some("  九哥  ".to_string()),
            Some("token-id".to_string()),
            None,
        )
        .unwrap();

    assert_eq!(manager.accounts()[0].display_name, "九哥");
}

#[test]
fn saved_account_falls_back_to_username_when_nickname_is_blank() {
    let dir = tempfile::tempdir().unwrap();
    let manager =
        AccountManager::with_store(AccountStore::new(dir.path().join("account-auth.bin")));
    manager
        .complete_token_login(
            "token",
            Some("alice".to_string()),
            Some("   ".to_string()),
            None,
            None,
        )
        .unwrap();

    assert_eq!(manager.accounts()[0].display_name, "alice");
    assert_eq!(manager.status().display_name.as_deref(), Some("alice"));
}

#[test]
fn loads_legacy_credentials_without_a_nickname_field() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("account-auth.bin");
    let legacy = serde_json::json!({
        "version": 3,
        "activeUsername": "alice",
        "accounts": [{
            "username": "alice",
            "credentials": {
                "accessToken": "legacy-token",
                "username": "alice",
                "authMethod": "personalAccessToken"
            },
            "lastLoginAt": 42,
            "bridgeSettings": {
                "repoNamespace": "",
                "repoName": "cnb-ai-bridge"
            }
        }]
    });
    let encrypted = dpapi::protect(serde_json::to_vec(&legacy).unwrap().as_slice()).unwrap();
    std::fs::write(&path, encrypted).unwrap();

    let manager = AccountManager::with_store(AccountStore::new(path));

    assert_eq!(manager.status().display_name.as_deref(), Some("alice"));
    assert_eq!(
        manager.status().avatar_url.as_deref(),
        Some("https://cnb.cool/users/alice/avatar/l")
    );
}

#[test]
fn activating_an_account_refreshes_its_nickname() {
    let dir = tempfile::tempdir().unwrap();
    let store = AccountStore::new(dir.path().join("account-auth.bin"));
    let manager = AccountManager::with_store(store.clone());
    manager
        .complete_token_login(
            "token",
            Some("alice".to_string()),
            Some("旧昵称".to_string()),
            None,
            None,
        )
        .unwrap();

    let status = manager
        .activate_account("alice", Some("新昵称".to_string()))
        .unwrap();

    assert_eq!(status.display_name.as_deref(), Some("新昵称"));
    assert_eq!(
        AccountManager::with_store(store).accounts()[0].display_name,
        "新昵称"
    );
}

#[test]
fn refreshing_profile_nickname_persists_without_replacing_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let store = AccountStore::new(dir.path().join("account-auth.bin"));
    let manager = AccountManager::with_store(store.clone());
    manager
        .complete_token_login(
            "token",
            Some("alice".to_string()),
            Some("旧昵称".to_string()),
            Some("token-id".to_string()),
            None,
        )
        .unwrap();

    let status = manager
        .update_profile_nickname("alice", Some("新昵称"))
        .unwrap();

    assert_eq!(status.display_name.as_deref(), Some("新昵称"));
    let reopened = AccountManager::with_store(store);
    let credentials = reopened.credentials_for("alice").unwrap();
    assert_eq!(credentials.access_token, "token");
    assert_eq!(credentials.token_id.as_deref(), Some("token-id"));
    assert_eq!(credentials.nickname.as_deref(), Some("新昵称"));
}

#[test]
fn refreshing_blank_nickname_falls_back_to_username() {
    let dir = tempfile::tempdir().unwrap();
    let manager =
        AccountManager::with_store(AccountStore::new(dir.path().join("account-auth.bin")));
    manager
        .complete_token_login(
            "token",
            Some("alice".to_string()),
            Some("旧昵称".to_string()),
            None,
            None,
        )
        .unwrap();

    let status = manager
        .update_profile_nickname("alice", Some("   "))
        .unwrap();

    assert_eq!(status.display_name.as_deref(), Some("alice"));
    assert_eq!(manager.credentials_for("alice").unwrap().nickname, None);
}

#[test]
fn failed_saved_account_login_keeps_the_display_name() {
    let dir = tempfile::tempdir().unwrap();
    let manager =
        AccountManager::with_store(AccountStore::new(dir.path().join("account-auth.bin")));
    manager
        .complete_token_login(
            "token",
            Some("alice".to_string()),
            Some("爱丽丝".to_string()),
            None,
            None,
        )
        .unwrap();
    manager.logout().unwrap();

    manager.begin_account_login("alice").unwrap();
    assert_eq!(manager.status().display_name.as_deref(), Some("爱丽丝"));

    manager.fail_account_login("alice", "offline");
    assert_eq!(manager.status().display_name.as_deref(), Some("爱丽丝"));
}

#[test]
fn persists_pat_token_id_across_rescans_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let store = AccountStore::new(dir.path().join("account-auth.bin"));
    let manager = AccountManager::with_store(store.clone());
    manager
        .complete_token_login(
            "first-token",
            Some("alice".to_string()),
            None,
            Some("token-1".to_string()),
            None,
        )
        .unwrap();
    manager
        .complete_token_login(
            "second-token",
            Some("alice".to_string()),
            None,
            Some("token-2".to_string()),
            None,
        )
        .unwrap();

    assert_eq!(
        manager
            .credentials_for("alice")
            .unwrap()
            .token_id
            .as_deref(),
        Some("token-2")
    );
    assert_eq!(
        AccountManager::with_store(store)
            .credentials_for("alice")
            .unwrap()
            .token_id
            .as_deref(),
        Some("token-2")
    );
}

#[test]
fn finds_saved_credentials_by_token_id() {
    let dir = tempfile::tempdir().unwrap();
    let manager =
        AccountManager::with_store(AccountStore::new(dir.path().join("account-auth.bin")));
    manager
        .complete_token_login(
            "saved-token",
            Some("alice".to_string()),
            None,
            Some("token-1".to_string()),
            None,
        )
        .unwrap();

    assert_eq!(
        manager
            .credentials_by_token_id("token-1")
            .unwrap()
            .access_token,
        "saved-token"
    );
    assert!(manager.credentials_by_token_id("missing").is_none());
}

#[test]
fn migrates_a_version_two_account_book_without_token_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("account-auth.bin");
    let settings = AccountBridgeSettings {
        repo_namespace: "alice-team".to_string(),
        repo_name: "alice-bridge".to_string(),
    };
    let version_two = AccountBook {
        version: 2,
        active_username: Some("alice".to_string()),
        accounts: vec![StoredAccount {
            username: "alice".to_string(),
            credentials: Credentials {
                access_token: "legacy-token".to_string(),
                username: Some("alice".to_string()),
                nickname: None,
                auth_method: AuthMethod::PersonalAccessToken,
                token_id: None,
            },
            last_login_at: 42,
            bridge_settings: Some(settings.clone()),
        }],
    };
    let encrypted = dpapi::protect(serde_json::to_vec(&version_two).unwrap().as_slice()).unwrap();
    std::fs::write(&path, encrypted).unwrap();

    let manager = AccountManager::with_store(AccountStore::new(path));
    let book = manager.book.lock().unwrap();

    assert_eq!(book.version, 3);
    assert_eq!(book.active_username.as_deref(), Some("alice"));
    assert_eq!(book.accounts[0].credentials.token_id, None);
    assert_eq!(book.accounts[0].bridge_settings.as_ref(), Some(&settings));
}

#[test]
fn account_book_round_trips_multiple_accounts_and_hides_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let store = AccountStore::new(dir.path().join("account-auth.bin"));
    let manager = AccountManager::with_store(store.clone());
    manager
        .complete_token_login("alice-token", Some("alice".to_string()), None, None, None)
        .unwrap();
    manager
        .complete_token_login("bob-token", Some("bob".to_string()), None, None, None)
        .unwrap();

    let raw = std::fs::read(store.path()).unwrap();
    let text = String::from_utf8_lossy(&raw);
    assert!(!text.contains("alice-token"));
    assert!(!text.contains("bob-token"));

    let reopened = AccountManager::with_store(store);
    let accounts = reopened.accounts();
    assert_eq!(accounts.len(), 2);
    assert_eq!(accounts[0].username, "bob");
    assert!(accounts[0].active);
    assert_eq!(accounts[1].username, "alice");
    assert!(!accounts[1].active);
    assert_eq!(reopened.status().username.as_deref(), Some("bob"));
}

#[test]
fn rescanning_same_username_replaces_token_and_preserves_settings() {
    let dir = tempfile::tempdir().unwrap();
    let manager =
        AccountManager::with_store(AccountStore::new(dir.path().join("account-auth.bin")));
    let settings = AccountBridgeSettings {
        repo_namespace: "alice-team".to_string(),
        repo_name: "my-bridge".to_string(),
    };
    manager
        .complete_token_login(
            "first-token",
            Some("Alice".to_string()),
            None,
            None,
            Some(settings.clone()),
        )
        .unwrap();
    manager
        .complete_token_login("second-token", Some("alice".to_string()), None, None, None)
        .unwrap();

    let accounts = manager.accounts();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].username, "Alice");
    assert_eq!(manager.active_bridge_settings(), Some(settings));
    assert_eq!(
        manager.active_credentials().unwrap().access_token,
        "second-token"
    );
}

#[test]
fn logout_keeps_accounts_and_clears_active_account() {
    let dir = tempfile::tempdir().unwrap();
    let store = AccountStore::new(dir.path().join("account-auth.bin"));
    let manager = AccountManager::with_store(store.clone());
    manager
        .complete_token_login("alice-token", Some("alice".to_string()), None, None, None)
        .unwrap();

    let status = manager.logout().unwrap();

    assert_eq!(status.state, AccountState::SignedOut);
    assert_eq!(manager.accounts().len(), 1);
    assert!(!manager.accounts()[0].active);
    assert!(manager.active_credentials().is_none());

    let reopened = AccountManager::with_store(store);
    assert_eq!(reopened.status().state, AccountState::SignedOut);
    assert_eq!(reopened.accounts().len(), 1);
}

#[test]
fn cancelled_scan_returns_to_signed_out_without_clearing_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let store = AccountStore::new(dir.path().join("account-auth.bin"));
    let manager = AccountManager::with_store(store.clone());
    manager
        .complete_token_login("alice-token", Some("alice".to_string()), None, None, None)
        .unwrap();
    manager.logout().unwrap();
    manager.begin_token_login();

    let status = manager.cancel_token_login();

    assert_eq!(status.state, AccountState::SignedOut);
    assert!(status.error.is_none());
    assert_eq!(manager.accounts().len(), 1);
    assert!(manager.active_credentials().is_none());
    let reopened = AccountManager::with_store(store);
    assert_eq!(reopened.status().state, AccountState::SignedOut);
    assert_eq!(reopened.accounts().len(), 1);
}

#[test]
fn account_bridge_settings_follow_the_active_account() {
    let dir = tempfile::tempdir().unwrap();
    let manager =
        AccountManager::with_store(AccountStore::new(dir.path().join("account-auth.bin")));
    manager
        .complete_token_login(
            "alice-token",
            Some("alice".to_string()),
            None,
            None,
            Some(AccountBridgeSettings {
                repo_namespace: "alice-team".to_string(),
                repo_name: "alice-bridge".to_string(),
            }),
        )
        .unwrap();
    manager
        .complete_token_login(
            "bob-token",
            Some("bob".to_string()),
            None,
            None,
            Some(AccountBridgeSettings {
                repo_namespace: "bob-team".to_string(),
                repo_name: "bob-bridge".to_string(),
            }),
        )
        .unwrap();

    manager.logout().unwrap();
    manager.activate_account("alice", None).unwrap();

    assert_eq!(
        manager.active_bridge_settings(),
        Some(AccountBridgeSettings {
            repo_namespace: "alice-team".to_string(),
            repo_name: "alice-bridge".to_string(),
        })
    );
    assert_eq!(
        manager.active_credentials().unwrap().access_token,
        "alice-token"
    );
}

#[test]
fn active_account_settings_updates_persist_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let store = AccountStore::new(dir.path().join("account-auth.bin"));
    let manager = AccountManager::with_store(store.clone());
    manager
        .complete_token_login("alice-token", Some("alice".to_string()), None, None, None)
        .unwrap();
    let settings = AccountBridgeSettings {
        repo_namespace: "alice-team".to_string(),
        repo_name: "updated-bridge".to_string(),
    };

    manager
        .update_active_bridge_settings(settings.clone())
        .unwrap();

    assert_eq!(
        AccountManager::with_store(store).active_bridge_settings(),
        Some(settings)
    );
}

#[test]
fn selecting_saved_account_updates_recent_login_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = AccountStore::new(dir.path().join("account-auth.bin"));
    let manager = AccountManager::with_store(store.clone());
    manager
        .complete_token_login(
            "alice-token",
            Some("alice".to_string()),
            None,
            None,
            Some(AccountBridgeSettings {
                repo_namespace: "alice-team".to_string(),
                repo_name: "alice-bridge".to_string(),
            }),
        )
        .unwrap();
    manager
        .complete_token_login(
            "bob-token",
            Some("bob".to_string()),
            None,
            None,
            Some(AccountBridgeSettings {
                repo_namespace: "bob-team".to_string(),
                repo_name: "bob-bridge".to_string(),
            }),
        )
        .unwrap();
    manager.logout().unwrap();

    manager.activate_account("alice", None).unwrap();
    manager.logout().unwrap();

    let accounts = manager.accounts();
    assert_eq!(
        accounts
            .iter()
            .map(|account| account.username.as_str())
            .collect::<Vec<_>>(),
        vec!["alice", "bob"]
    );
    assert_eq!(
        manager.credentials_for("alice").unwrap().access_token,
        "alice-token"
    );
    assert_eq!(
        manager.active_bridge_settings(),
        None,
        "logging out should not select a new active account"
    );

    let reopened = AccountManager::with_store(store);
    assert_eq!(
        reopened
            .accounts()
            .iter()
            .map(|account| account.username.as_str())
            .collect::<Vec<_>>(),
        vec!["alice", "bob"]
    );
}

#[test]
fn active_account_cannot_be_removed_before_logout() {
    let dir = tempfile::tempdir().unwrap();
    let manager =
        AccountManager::with_store(AccountStore::new(dir.path().join("account-auth.bin")));
    manager
        .complete_token_login("alice-token", Some("alice".to_string()), None, None, None)
        .unwrap();

    assert!(manager.remove_account("alice").is_err());

    manager.logout().unwrap();
    manager.remove_account("alice").unwrap();
    assert!(manager.accounts().is_empty());
}

#[test]
fn rejects_legacy_oauth_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("account-auth.bin");
    let legacy = serde_json::json!({
        "accessToken": "oauth-token",
        "refreshToken": "oauth-refresh",
        "expiresAtMs": 1_800_000_000_000_i64,
        "username": "legacy-user"
    });
    let encrypted = dpapi::protect(serde_json::to_string(&legacy).unwrap().as_bytes()).unwrap();
    std::fs::write(&path, encrypted).unwrap();
    let store = AccountStore::new(&path);
    let manager = AccountManager::with_store(store);

    assert_eq!(manager.status().state, AccountState::Failed);
    assert!(manager.accounts().is_empty());
    assert!(!path.exists());
}

#[test]
fn migrates_legacy_pat_to_active_account() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("account-auth.bin");
    let credentials = Credentials {
        access_token: "legacy-token".to_string(),
        username: Some("legacy-user".to_string()),
        nickname: None,
        auth_method: AuthMethod::PersonalAccessToken,
        token_id: None,
    };
    let encrypted = dpapi::protect(serde_json::to_vec(&credentials).unwrap().as_slice()).unwrap();
    std::fs::write(&path, encrypted).unwrap();
    let store = AccountStore::new(&path);

    let manager = AccountManager::with_store(store.clone());

    assert_eq!(manager.status().state, AccountState::Connected);
    assert_eq!(manager.status().username.as_deref(), Some("legacy-user"));
    assert_eq!(manager.accounts().len(), 1);
    assert!(manager.active_bridge_settings().is_none());
    assert_eq!(
        manager.active_credentials().unwrap().access_token,
        "legacy-token"
    );

    let reopened = AccountManager::with_store(store);
    assert_eq!(reopened.accounts().len(), 1);
}

#[test]
fn organization_requirement_is_preserved_only_for_connected_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let manager =
        AccountManager::with_store(AccountStore::new(dir.path().join("account-auth.bin")));
    manager.begin_token_login();

    let status = manager.set_needs_organization(true);
    assert!(!status.needs_organization);

    let status = manager
        .complete_token_login(
            "personal-access-token",
            Some("new-user".to_string()),
            None,
            None,
            None,
        )
        .unwrap();
    assert!(!status.needs_organization);

    let status = manager.set_needs_organization(true);
    assert!(status.needs_organization);

    let status = manager.set_needs_organization(false);
    assert!(!status.needs_organization);
}

#[test]
fn identity_verification_failure_carries_action_and_is_cleared_on_login() {
    let dir = tempfile::tempdir().unwrap();
    let manager =
        AccountManager::with_store(AccountStore::new(dir.path().join("account-auth.bin")));
    let action = AccountAction {
        label: "实名认证 · 个人设置".to_string(),
        url: "https://cnb.cool/profile/auth".to_string(),
    };

    let status = manager.fail_token_login_with_action("请先完成实名认证", action.clone());

    assert_eq!(status.error_action, Some(action.clone()));
    assert_eq!(status.error.as_deref(), Some("请先完成实名认证"));

    let status = manager
        .complete_token_login("token", Some("test-user".to_string()), None, None, None)
        .unwrap();
    assert_eq!(status.error_action, None);
}

#[test]
fn dismissing_an_account_error_preserves_the_failed_state_and_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let manager =
        AccountManager::with_store(AccountStore::new(dir.path().join("account-auth.bin")));
    manager
        .complete_token_login(
            "token",
            Some("alice".to_string()),
            Some("爱丽丝".to_string()),
            None,
            None,
        )
        .unwrap();
    manager.logout().unwrap();
    manager.fail_token_login_with_action(
        "扫码窗口已关闭",
        AccountAction {
            label: "实名认证".to_string(),
            url: "https://cnb.cool/profile/auth".to_string(),
        },
    );

    let status = manager.dismiss_error();

    assert_eq!(status.state, AccountState::Failed);
    assert_eq!(status.error, None);
    assert_eq!(status.error_action, None);
    assert_eq!(manager.accounts().len(), 1);
    assert_eq!(manager.accounts()[0].display_name, "爱丽丝");
    assert!(manager.active_credentials().is_none());
}
