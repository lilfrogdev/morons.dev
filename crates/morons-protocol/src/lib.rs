pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hello {
    pub protocol_version: u32,
}

impl Hello {
    #[must_use]
    pub const fn current() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Hello, PROTOCOL_VERSION};

    #[test]
    fn current_hello_uses_current_protocol_version() {
        assert_eq!(Hello::current().protocol_version, PROTOCOL_VERSION);
    }
}
