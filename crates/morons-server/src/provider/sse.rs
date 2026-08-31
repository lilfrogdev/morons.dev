use super::ProviderError;

pub(super) const MAX_SSE_RECORD_BYTES: usize = 1024 * 1024;
pub(super) const MAX_PROVIDER_STREAM_BYTES: usize = 16 * 1024 * 1024;
const MAX_SSE_EVENT_NAME_BYTES: usize = 128;

pub(super) struct SseRecord {
    pub(super) event: Option<String>,
    pub(super) data: Vec<u8>,
}

pub(super) struct SseDecoder {
    line: Vec<u8>,
    event: Option<String>,
    data: Vec<u8>,
    data_lines: usize,
    record_bytes: usize,
    stream_bytes: usize,
}

impl SseDecoder {
    pub(super) fn new() -> Self {
        Self {
            line: Vec::new(),
            event: None,
            data: Vec::new(),
            data_lines: 0,
            record_bytes: 0,
            stream_bytes: 0,
        }
    }

    pub(super) fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseRecord>, ProviderError> {
        self.stream_bytes = self
            .stream_bytes
            .checked_add(chunk.len())
            .filter(|bytes| *bytes <= MAX_PROVIDER_STREAM_BYTES)
            .ok_or(ProviderError::ResponseLimitExceeded)?;
        let mut records = Vec::new();
        for byte in chunk {
            self.record_bytes = self
                .record_bytes
                .checked_add(1)
                .filter(|bytes| *bytes <= MAX_SSE_RECORD_BYTES)
                .ok_or(ProviderError::ResponseLimitExceeded)?;
            if *byte == b'\n' {
                let mut line = std::mem::take(&mut self.line);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if let Some(record) = self.process_line(&line)? {
                    records.push(record);
                }
            } else {
                self.line.push(*byte);
            }
        }
        Ok(records)
    }

    pub(super) fn finish(self) -> Result<(), ProviderError> {
        if self.line.is_empty()
            && self.event.is_none()
            && self.data.is_empty()
            && self.data_lines == 0
            && self.record_bytes == 0
        {
            Ok(())
        } else {
            Err(ProviderError::IncompleteResponse)
        }
    }

    fn process_line(&mut self, line: &[u8]) -> Result<Option<SseRecord>, ProviderError> {
        if line.is_empty() {
            self.record_bytes = 0;
            if self.event.is_none() && self.data_lines == 0 {
                return Ok(None);
            }
            if self.data_lines == 0 {
                return Err(ProviderError::MalformedResponse);
            }
            self.data_lines = 0;
            return Ok(Some(SseRecord {
                event: self.event.take(),
                data: std::mem::take(&mut self.data),
            }));
        }
        if line.starts_with(b":") {
            return Ok(None);
        }
        let Some(separator) = line.iter().position(|byte| *byte == b':') else {
            return Err(ProviderError::MalformedResponse);
        };
        let field = &line[..separator];
        let mut value = &line[separator + 1..];
        if value.first() == Some(&b' ') {
            value = &value[1..];
        }
        match field {
            b"event" => {
                if self.event.is_some()
                    || value.is_empty()
                    || value.len() > MAX_SSE_EVENT_NAME_BYTES
                {
                    return Err(ProviderError::MalformedResponse);
                }
                let event =
                    std::str::from_utf8(value).map_err(|_| ProviderError::MalformedResponse)?;
                if event.bytes().any(|byte| !(0x21..=0x7e).contains(&byte)) {
                    return Err(ProviderError::MalformedResponse);
                }
                self.event = Some(event.to_owned());
            }
            b"data" => {
                if self.data_lines > 0 {
                    self.data.push(b'\n');
                }
                self.data.extend_from_slice(value);
                self.data_lines = self
                    .data_lines
                    .checked_add(1)
                    .ok_or(ProviderError::ResponseLimitExceeded)?;
                if self.data.len() > MAX_SSE_RECORD_BYTES {
                    return Err(ProviderError::ResponseLimitExceeded);
                }
            }
            _ => return Err(ProviderError::MalformedResponse),
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::SseDecoder;
    use crate::provider::ProviderError;

    #[test]
    fn decoder_handles_split_crlf_records_and_comments() {
        let mut decoder = SseDecoder::new();
        assert!(
            decoder
                .push(b": keep-alive\r\nevent: response.created\r\nda")
                .expect("first chunk should decode")
                .is_empty()
        );
        let records = decoder
            .push(b"ta: {\"type\":\"response.created\"}\r\n\r\n")
            .expect("second chunk should decode");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].event.as_deref(), Some("response.created"));
        assert_eq!(records[0].data, br#"{"type":"response.created"}"#);
        decoder.finish().expect("complete stream should finish");
    }

    #[test]
    fn decoder_rejects_unknown_fields_incomplete_records_and_limits() {
        let mut unknown = SseDecoder::new();
        assert_eq!(
            unknown.push(b"retry: 1000\n\n").err(),
            Some(ProviderError::MalformedResponse)
        );

        let mut incomplete = SseDecoder::new();
        incomplete
            .push(b"data: {}")
            .expect("partial record should be buffered");
        assert_eq!(incomplete.finish(), Err(ProviderError::IncompleteResponse));

        let mut oversized = SseDecoder::new();
        assert_eq!(
            oversized
                .push(&vec![b'x'; super::MAX_SSE_RECORD_BYTES + 1])
                .err(),
            Some(ProviderError::ResponseLimitExceeded)
        );
    }
}
