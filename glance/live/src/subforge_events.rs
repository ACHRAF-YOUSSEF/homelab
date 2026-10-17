//! Incremental SSE framing with a hard cap on every record, including ignored fields.
use crate::Error;

pub(crate) struct Decoder {
    limit: usize,
    record_bytes: usize,
    line: Vec<u8>,
    data: Vec<u8>,
    event: String,
    after_cr: bool,
    first_line: bool,
}

impl Decoder {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            record_bytes: 0,
            line: Vec::new(),
            data: Vec::new(),
            event: String::new(),
            after_cr: false,
            first_line: true,
        }
    }

    pub(crate) fn feed(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
        if chunk.len() > self.limit {
            return Err(Error::Limit);
        }
        let mut frames = Vec::new();
        for &byte in chunk {
            if byte == b'\n' && self.after_cr {
                self.after_cr = false;
                continue;
            }
            self.after_cr = byte == b'\r';
            self.record_bytes += 1;
            if self.record_bytes > self.limit {
                return Err(Error::Limit);
            }
            if matches!(byte, b'\r' | b'\n') {
                if self.first_line {
                    self.first_line = false;
                    if self.line.starts_with(&[0xef, 0xbb, 0xbf]) {
                        self.line.drain(..3);
                    }
                }
                if self.line.is_empty() {
                    if self.event == "widget" && !self.data.is_empty() {
                        self.data.pop();
                        frames.push(std::mem::take(&mut self.data));
                    } else {
                        self.data.clear();
                    }
                    self.event.clear();
                    self.record_bytes = 0;
                } else if !self.line.starts_with(b":") {
                    let separator = self
                        .line
                        .iter()
                        .position(|&byte| byte == b':')
                        .unwrap_or(self.line.len());
                    let field = &self.line[..separator];
                    let value = self.line.get(separator + 1..).unwrap_or_default();
                    let value = value.strip_prefix(b" ").unwrap_or(value);
                    if field == b"event" {
                        if value.len() > 64 {
                            return Err(Error::Limit);
                        }
                        self.event = std::str::from_utf8(value)
                            .map_err(|_| Error::InvalidData)?
                            .to_owned();
                    } else if field == b"data" {
                        if value.len() + 1 > self.limit.saturating_sub(self.data.len()) {
                            return Err(Error::Limit);
                        }
                        self.data.extend_from_slice(value);
                        self.data.push(b'\n');
                    }
                }
                self.line.clear();
            } else {
                self.line.push(byte);
            }
        }
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_chunks_crlf_comments_multiline_and_bom_preserve_widget_frames() {
        let raw = b"\xef\xbb\xbf: ready\r\nevent: widget\r\ndata: {\r\ndata: \"active_jobs\":[]}\r\n\r\nevent: status\ndata: secret dashboard\n\nevent: widget\rdata: {}\r\r";
        let mut decoder = Decoder::new(1024);
        let mut frames = Vec::new();
        for byte in raw {
            frames.extend(decoder.feed(&[*byte]).unwrap());
        }
        assert_eq!(frames, [b"{\n\"active_jobs\":[]}".to_vec(), b"{}".to_vec()]);
    }

    #[test]
    fn unfinished_records_and_ignored_fields_cannot_grow_without_bound() {
        let mut decoder = Decoder::new(16);
        assert!(decoder.feed(b":abcdefghijklmno").is_ok());
        assert!(decoder.feed(b"p").is_err());
        assert!(Decoder::new(16).feed(&[b'x'; 17]).is_err());
        let mut decoder = Decoder::new(64);
        assert!(
            decoder
                .feed(b"event: widget\ndata: incomplete")
                .unwrap()
                .is_empty()
        );
        assert!(decoder.feed(b"\n").unwrap().is_empty());
        assert_eq!(decoder.feed(b"\n").unwrap(), [b"incomplete".to_vec()]);
    }
}
