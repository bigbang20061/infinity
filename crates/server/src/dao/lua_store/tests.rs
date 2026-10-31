use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn real_db_isolates_script_keys_and_rolls_back() {
    let (mut client, connection) = tokio_postgres::connect(&crate::config::tests::test_database().url, tokio_postgres::NoTls).await.unwrap();
    let driver = tokio::spawn(async move { connection.await.unwrap() });
    let tx = client.transaction().await.unwrap();
    let a = format!("lua_test_{}_a", std::process::id());
    let b = format!("lua_test_{}_b", std::process::id());
    assert_eq!(get(&tx, &a, "key").await.unwrap(), None);
    set(&tx, &a, "key", "{\"nested\":[1,true]}").await.unwrap();
    set(&tx, &b, "key", "\"other\"").await.unwrap();
    assert_eq!(get(&tx, &a, "key").await.unwrap().unwrap(), "{\"nested\": [1, true]}");
    assert_eq!(get(&tx, &b, "key").await.unwrap().unwrap(), "\"other\"");
    assert_eq!(keys(&tx, &a, "ke").await.unwrap(), vec!["key"]);
    assert!(delete(&tx, &a, "key").await.unwrap());
    assert_eq!(get(&tx, &a, "key").await.unwrap(), None);
    assert!(get(&tx, &b, "key").await.unwrap().is_some());
    tx.rollback().await.unwrap();
    drop(client);
    driver.await.unwrap();
}
