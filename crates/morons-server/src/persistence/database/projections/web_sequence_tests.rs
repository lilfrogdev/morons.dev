use super::validate_logical_sequences;
use crate::persistence::database::*;

#[test]
fn web_sequences_reject_collisions_and_unallocated_values() {
    let connection = schema_29_fixture();
    for schema in [
        SCHEMA_V30,
        SCHEMA_V31,
        SCHEMA_V32,
        SCHEMA_V33,
        SCHEMA_V34,
        SCHEMA_V34_SEARCH,
        SCHEMA_V35,
        SCHEMA_V36,
        SCHEMA_V37,
        SCHEMA_V38,
        SCHEMA_V39,
        SCHEMA_V40,
        SCHEMA_V41,
        SCHEMA_V42,
        SCHEMA_V43,
        SCHEMA_V44,
        SCHEMA_V45,
    ] {
        connection.execute_batch(schema).unwrap();
    }
    connection
        .set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_COMPOUND_SELECT, 16)
        .unwrap();
    // Isolate sequence validation from foreign-key and payload validation.
    connection
        .execute_batch(
            "PRAGMA foreign_keys=OFF;
             UPDATE logical_sequences SET next_value=100;
             INSERT INTO web_search_attempts VALUES(zeroblob(16),0,0,zeroblob(32),zeroblob(32),0,1,0,10);
             INSERT INTO web_search_successes VALUES(zeroblob(16),0,0,x'00',20,zeroblob(32));",
        )
        .unwrap();
    validate_logical_sequences(&connection).unwrap();
    for corruption in [
        "UPDATE web_search_successes SET completion_sequence=10",
        "UPDATE web_search_successes SET completion_sequence=100",
        "UPDATE web_search_attempts SET dispatch_sequence=100",
        "INSERT INTO server_stop_requests VALUES(zeroblob(16),zeroblob(32),zeroblob(16),0,10,0)",
        "INSERT INTO server_stop_requests VALUES(zeroblob(16),zeroblob(32),zeroblob(16),0,20,0)",
        "INSERT INTO data_use_policies VALUES(zeroblob(16),zeroblob(32),0,10,0,0,0)",
        "INSERT INTO data_use_policies VALUES(zeroblob(16),zeroblob(32),0,20,0,0,0)",
        "INSERT INTO tool_uncertainty_acknowledgements VALUES(zeroblob(16),zeroblob(32),zeroblob(16),zeroblob(16),10,0,zeroblob(16))",
        "INSERT INTO tool_uncertainty_acknowledgements VALUES(zeroblob(16),zeroblob(32),zeroblob(16),zeroblob(16),20,0,zeroblob(16))",
    ] {
        connection.execute_batch("SAVEPOINT corruption").unwrap();
        connection.execute(corruption, []).unwrap();
        assert!(
            validate_logical_sequences(&connection).is_err(),
            "{corruption}"
        );
        connection
            .execute_batch("ROLLBACK TO corruption; RELEASE corruption")
            .unwrap();
        validate_logical_sequences(&connection).unwrap();
    }
}
