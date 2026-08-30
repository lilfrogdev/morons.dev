use std::{error::Error, fmt};

use morons_protocol::{APPLICATION_IDENTIFIER_BYTES, MutationRequestId};

#[derive(Debug)]
#[non_exhaustive]
pub enum MutationRequestIdError {
    Randomness(getrandom::Error),
    InvalidRandomOutput,
}

impl fmt::Display for MutationRequestIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Randomness(error) => {
                write!(formatter, "mutation request randomness failed: {error}")
            }
            Self::InvalidRandomOutput => {
                formatter.write_str("mutation request randomness produced an invalid identifier")
            }
        }
    }
}

impl Error for MutationRequestIdError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Randomness(error) => Some(error),
            Self::InvalidRandomOutput => None,
        }
    }
}

pub fn generate_mutation_request_id() -> Result<MutationRequestId, MutationRequestIdError> {
    let mut bytes = [0_u8; APPLICATION_IDENTIFIER_BYTES];
    getrandom::fill(&mut bytes).map_err(MutationRequestIdError::Randomness)?;
    if bytes.iter().all(|byte| *byte == 0) {
        return Err(MutationRequestIdError::InvalidRandomOutput);
    }
    Ok(MutationRequestId::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::generate_mutation_request_id;

    #[test]
    fn generated_mutation_request_identifier_is_not_zero() {
        let request_id = generate_mutation_request_id()
            .expect("operating-system randomness should be available");
        assert!(request_id.as_bytes().iter().any(|byte| *byte != 0));
    }
}
