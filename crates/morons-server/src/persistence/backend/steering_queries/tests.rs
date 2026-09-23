use super::*;

#[test]
fn steering_target_admission_rejects_uncertainty_even_after_success() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE runs (session_id BLOB, run_id BLOB, state INTEGER,
                cancellation_requested INTEGER);
             CREATE TABLE provider_operation_facts (run_id BLOB, fact_kind INTEGER);",
        )
        .unwrap();
    let session = SessionId::from_bytes([1; 16]);
    let run = RunId::from_bytes([2; 16]);
    let other = RunId::from_bytes([3; 16]);
    connection
        .execute(
            "INSERT INTO runs VALUES (?1, ?2, 2, 0)",
            params![session.as_bytes(), run.as_bytes()],
        )
        .unwrap();
    assert!(target_accepts_steering(&connection, session, run).unwrap());
    assert!(!target_accepts_steering(&connection, session, other).unwrap());
    assert!(!target_accepts_steering(&connection, SessionId::from_bytes([4; 16]), run).unwrap());
    connection
        .execute(
            "INSERT INTO provider_operation_facts VALUES (?1, 5)",
            [other.as_bytes()],
        )
        .unwrap();
    assert!(target_accepts_steering(&connection, session, run).unwrap());
    connection
        .execute(
            "INSERT INTO provider_operation_facts VALUES (?1, 5)",
            [run.as_bytes()],
        )
        .unwrap();
    assert!(!target_accepts_steering(&connection, session, run).unwrap());
    connection
        .execute(
            "INSERT INTO provider_operation_facts VALUES (?1, 3)",
            [run.as_bytes()],
        )
        .unwrap();
    assert!(!target_accepts_steering(&connection, session, run).unwrap());
}

#[test]
fn steering_target_admission_requires_live_uncancelled_run() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE runs (session_id BLOB, run_id BLOB, state INTEGER,
                cancellation_requested INTEGER);
             CREATE TABLE provider_operation_facts (run_id BLOB, fact_kind INTEGER);",
        )
        .unwrap();
    let session = SessionId::from_bytes([1; 16]);
    let run = RunId::from_bytes([2; 16]);
    for state in 1..=7 {
        for cancelled in [false, true] {
            connection.execute("DELETE FROM runs", []).unwrap();
            connection
                .execute(
                    "INSERT INTO runs VALUES (?1, ?2, ?3, ?4)",
                    params![session.as_bytes(), run.as_bytes(), state, cancelled],
                )
                .unwrap();
            assert_eq!(
                target_accepts_steering(&connection, session, run).unwrap(),
                state <= 2 && !cancelled,
                "state={state}, cancelled={cancelled}"
            );
        }
    }
}
