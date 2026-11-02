use holtmail_store_sqlite::SqliteStore;

use super::*;

const CLIENT: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 7));

struct Fixture {
    auth: Auth,
    store: Arc<SqliteStore>,
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
    let auth = Auth::new(
        store.clone(),
        Options {
            max_failures: 3,
            hash_cost: HashCost {
                memory_kib: 64,
                iterations: 1,
                lanes: 1,
            },
            ..Options::default()
        },
    )
    .unwrap();
    Fixture {
        auth,
        store,
        _dir: dir,
    }
}

fn ada() -> Signup {
    Signup {
        email: "Ada@Example.com".into(),
        name: "Ada Lovelace".into(),
        password: "analytical engine".into(),
    }
}

#[tokio::test]
async fn setup_creates_the_owner_once() {
    let f = fixture().await;
    assert!(f.auth.setup_needed().await.unwrap());
    assert!(matches!(
        f.auth.complete_setup("anything", ada()).await,
        Err(AuthError::InvalidSetupToken)
    ));
    let token = f.auth.setup_token().await.unwrap().unwrap();
    assert_eq!(
        f.auth.setup_token().await.unwrap().as_deref(),
        Some(&*token)
    );
    assert!(matches!(
        f.auth.complete_setup(&random_token().unwrap(), ada()).await,
        Err(AuthError::InvalidSetupToken)
    ));
    let mut short = ada();
    short.password = "too short".into();
    assert!(matches!(
        f.auth.complete_setup(&token, short).await,
        Err(AuthError::Invalid(_))
    ));

    let signed_in = f.auth.complete_setup(&token, ada()).await.unwrap();
    assert_eq!(signed_in.user.role, InstanceRole::Owner);
    assert_eq!(signed_in.user.email, "Ada@Example.com");
    let session_user = f.auth.session(&signed_in.token).await.unwrap();
    assert_eq!(session_user, Some(signed_in.user.clone()));
    let hash = f
        .store
        .password_hash(signed_in.user.id)
        .await
        .unwrap()
        .unwrap();
    assert!(hash.starts_with("$argon2id$"), "password stored in clear");

    assert!(!f.auth.setup_needed().await.unwrap());
    assert_eq!(f.auth.setup_token().await.unwrap(), None);
    assert!(matches!(
        f.auth.complete_setup(&token, ada()).await,
        Err(AuthError::InvalidSetupToken)
    ));
}

#[tokio::test]
async fn setup_closes_when_a_user_appears_elsewhere() {
    let f = fixture().await;
    let token = f.auth.setup_token().await.unwrap().unwrap();
    f.auth
        .create_user(ada(), InstanceRole::Owner)
        .await
        .unwrap();
    let mut other = ada();
    other.email = "bob@example.org".into();
    assert!(matches!(
        f.auth.complete_setup(&token, other).await,
        Err(AuthError::InvalidSetupToken)
    ));
}

#[tokio::test]
async fn signs_in_and_out() {
    let f = fixture().await;
    f.auth
        .create_user(ada(), InstanceRole::Member)
        .await
        .unwrap();
    assert!(matches!(
        f.auth.create_user(ada(), InstanceRole::Member).await,
        Err(AuthError::EmailTaken)
    ));

    let signed_in = f
        .auth
        .login(CLIENT, " ada@example.COM ", "analytical engine")
        .await
        .unwrap();
    assert_eq!(signed_in.token.len(), 43);
    let lifetime = signed_in.expires_at.as_millis() - Timestamp::now().as_millis();
    assert!(lifetime > 29 * 24 * 3600 * 1000, "{lifetime}");
    assert!(f.auth.session(&signed_in.token).await.unwrap().is_some());

    f.auth.logout(&signed_in.token).await.unwrap();
    f.auth.logout(&signed_in.token).await.unwrap();
    f.auth.logout("garbage").await.unwrap();
    assert_eq!(f.auth.session(&signed_in.token).await.unwrap(), None);
    assert_eq!(f.auth.session("garbage").await.unwrap(), None);
}

#[tokio::test]
async fn rejects_wrong_passwords_and_unknown_emails_alike() {
    let f = fixture().await;
    f.auth
        .create_user(ada(), InstanceRole::Member)
        .await
        .unwrap();
    for (email, password) in [
        ("ada@example.com", "wrong password"),
        ("nobody@example.com", "analytical engine"),
    ] {
        let error = f.auth.login(CLIENT, email, password).await.unwrap_err();
        assert!(matches!(error, AuthError::InvalidCredentials), "{email}");
        assert_eq!(error.to_string(), "wrong email or password");
    }
}

#[tokio::test]
async fn limits_failed_sign_ins_per_client_and_email() {
    let f = fixture().await;
    f.auth
        .create_user(ada(), InstanceRole::Member)
        .await
        .unwrap();
    for _ in 0..3 {
        assert!(matches!(
            f.auth
                .login(CLIENT, "ada@example.com", "wrong password")
                .await,
            Err(AuthError::InvalidCredentials)
        ));
    }
    assert!(matches!(
        f.auth
            .login(CLIENT, "ADA@example.com", "analytical engine")
            .await,
        Err(AuthError::TooManyAttempts { retry_after }) if retry_after > Duration::from_secs(800)
    ));
    let elsewhere = IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, 1));
    f.auth
        .login(elsewhere, "ada@example.com", "analytical engine")
        .await
        .unwrap();
}

#[tokio::test]
async fn a_new_password_ends_every_session() {
    let f = fixture().await;
    let user = f
        .auth
        .create_user(ada(), InstanceRole::Member)
        .await
        .unwrap();
    let old = f
        .auth
        .login(CLIENT, "ada@example.com", "analytical engine")
        .await
        .unwrap();
    assert!(matches!(
        f.auth.set_password(user.id, "short").await,
        Err(AuthError::Invalid(_))
    ));
    f.auth
        .set_password(user.id, "difference engine")
        .await
        .unwrap();
    assert_eq!(f.auth.session(&old.token).await.unwrap(), None);
    assert!(
        f.auth
            .login(CLIENT, "ada@example.com", "analytical engine")
            .await
            .is_err()
    );
    f.auth
        .login(CLIENT, "ada@example.com", "difference engine")
        .await
        .unwrap();
    assert_eq!(f.auth.prune().await.unwrap(), 0);
}
