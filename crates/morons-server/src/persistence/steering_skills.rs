use sha2::{Digest as _, Sha256};

use super::PersistenceError;
use crate::skills::RunSkillContext;

const MAX_BYTES: usize = 2 * 1024 * 1024;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u8,
    skills: RunSkillContext,
}

pub(super) fn encode(
    skills: RunSkillContext,
    fingerprint: &[u8; 32],
) -> Result<(String, [u8; 32]), PersistenceError> {
    if !skills.is_valid() {
        return Err(invalid());
    }
    let text = serde_json::to_string(&Snapshot { version: 1, skills }).map_err(|_| invalid())?;
    if text.len() > MAX_BYTES {
        return Err(invalid());
    }
    let digest = digest(&text, fingerprint);
    Ok((text, digest))
}

fn digest(text: &str, fingerprint: &[u8; 32]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"morons-steering-skills-v1\0");
    hash.update(fingerprint);
    hash.update(text.as_bytes());
    hash.finalize().into()
}

fn decode(
    text: &str,
    stored: &[u8; 32],
    fingerprint: &[u8; 32],
) -> Result<RunSkillContext, PersistenceError> {
    if text.is_empty() || text.len() > MAX_BYTES || digest(text, fingerprint) != *stored {
        return Err(invalid());
    }
    let snapshot: Snapshot = serde_json::from_str(text).map_err(|_| invalid())?;
    if snapshot.version != 1 || !snapshot.skills.is_valid() {
        return Err(invalid());
    }
    Ok(snapshot.skills)
}

pub(super) fn validate(connection: &rusqlite::Connection) -> Result<(), PersistenceError> {
    let mut statement = connection.prepare(
        "SELECT change_kind, operation_fingerprint, skill_context, skill_context_digest, text
         FROM steering_mutation_requests
         WHERE skill_context IS NOT NULL OR skill_context_digest IS NOT NULL",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let kind: i64 = row.get(0)?;
        let fingerprint: [u8; 32] = row.get(1)?;
        let text = row.get_ref(2)?.as_str().map_err(|_| invalid())?;
        let stored: [u8; 32] = row.get(3).map_err(|_| invalid())?;
        if !matches!(kind, 1 | 2) {
            return Err(invalid());
        }
        let skills = decode(text, &stored, &fingerprint)?;
        let prompt = row.get_ref(4)?.as_str().map_err(|_| invalid())?;
        if !skills.invocations_match(prompt) {
            return Err(invalid());
        }
    }
    let delivered: bool = connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM steering_delivery_facts AS delivery
         JOIN steering_mutation_requests AS mutation
           ON mutation.item_id = delivery.item_id AND mutation.item_revision = delivery.item_revision
         WHERE mutation.change_kind IN (1, 2) AND mutation.skill_context IS NOT NULL)",
        [], |row| row.get(0),
    )?;
    if delivered {
        return Err(invalid());
    }
    Ok(())
}

fn invalid() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "stored steering skill context is invalid",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steering_skill_validation_rejects_delivery_and_implicit_activation() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE steering_mutation_requests (
            change_kind, operation_fingerprint, skill_context, skill_context_digest,
            text, item_id, item_revision);
            CREATE TABLE steering_delivery_facts (item_id, item_revision);",
        )
        .unwrap();
        let skills = crate::skills::SkillDiscovery::for_test(vec![]).context(
            std::path::Path::new("/nonexistent-steering-test"),
            "@skill-creator",
        );
        let (text, digest) = encode(skills, &[1; 32]).unwrap();
        db.execute(
            "INSERT INTO steering_mutation_requests VALUES (1, ?1, ?2, ?3, '@skill-creator', 1, 1)",
            rusqlite::params![[1_u8; 32], text, digest],
        )
        .unwrap();
        validate(&db).unwrap();
        db.execute(
            "UPDATE steering_mutation_requests SET text = 'ordinary text'",
            [],
        )
        .unwrap();
        assert!(validate(&db).is_err());
        db.execute(
            "UPDATE steering_mutation_requests SET text = '@skill-creator'",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO steering_delivery_facts VALUES (1, 1)", [])
            .unwrap();
        assert!(validate(&db).is_err());
    }

    #[test]
    fn steering_skill_encoding_is_bounded_versioned_and_mutation_bound() {
        let fingerprint = [1; 32];
        let (text, stored) = encode(RunSkillContext::default(), &fingerprint).unwrap();
        assert_eq!(
            decode(&text, &stored, &fingerprint).unwrap(),
            RunSkillContext::default()
        );
        assert!(decode(&text, &stored, &[2; 32]).is_err());
        assert!(decode(&text, &[0; 32], &fingerprint).is_err());
        for text in [
            text.replace("\"version\":1", "\"version\":2"),
            "{\"version\":1,\"skills\":{\"skills\":[]},\"unknown\":true}".into(),
            "{".into(),
            " ".repeat(MAX_BYTES + 1),
        ] {
            assert!(decode(&text, &digest(&text, &fingerprint), &fingerprint).is_err());
        }
        let mut skills = crate::skills::SkillDiscovery::for_test(vec![]).context(
            std::path::Path::new("/nonexistent-steering-test"),
            "@skill-creator",
        );
        assert!(skills.invocations_match("@skill-creator"));
        assert!(!skills.invocations_match("ordinary text"));
        skills.skills[0].instructions = None;
        assert!(encode(skills, &fingerprint).is_err());
    }
}
