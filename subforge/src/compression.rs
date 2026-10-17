//! Bounded compression for complete HTTP replies. Event streams bypass this path.
use flate2::{Compression, write::GzEncoder};
use std::io::Write;

const MIN_BYTES: usize = 1024;
const MAX_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn accepts_gzip(header: Option<&str>) -> bool {
    accepts(header, "gzip")
}

pub(crate) fn accepts_identity(header: Option<&str>) -> bool {
    accepts(header, "identity")
}

fn accepts(header: Option<&str>, requested: &str) -> bool {
    let mut wildcard = None;
    for entry in header.unwrap_or_default().split(',') {
        let mut fields = entry.split(';');
        let coding = fields.next().unwrap_or_default().trim();
        let mut quality = 1.0_f32;
        for parameter in fields {
            if let Some((name, value)) = parameter.trim().split_once('=')
                && name.trim().eq_ignore_ascii_case("q")
            {
                quality = value.trim().parse().unwrap_or(0.0);
            }
        }
        let accepted = quality.is_finite() && quality > 0.0 && quality <= 1.0;
        if coding.eq_ignore_ascii_case(requested) {
            return accepted;
        }
        if coding == "*" {
            wildcard = Some(accepted);
        }
    }
    wildcard.unwrap_or(requested == "identity")
}

pub(crate) fn eligible(content: &str, bytes: usize) -> bool {
    (MIN_BYTES..=MAX_BYTES).contains(&bytes)
        && (content.starts_with("text/html")
            || content.starts_with("text/css")
            || content.starts_with("text/javascript")
            || content.starts_with("application/json")
            || content.starts_with("image/svg+xml"))
}

pub(crate) fn gzip(body: &[u8]) -> Option<Vec<u8>> {
    if !(MIN_BYTES..=MAX_BYTES).contains(&body.len()) {
        return None;
    }
    let mut encoder = GzEncoder::new(
        Vec::with_capacity(body.len().min(65536)),
        if body.len() <= 256 * 1024 {
            Compression::default()
        } else {
            Compression::fast()
        },
    );
    encoder.write_all(body).ok()?;
    let encoded = encoder.finish().ok()?;
    (encoded.len() < body.len()).then_some(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn encoding_negotiation_respects_explicit_rejection_and_quality() {
        for header in [
            "gzip",
            "br, GZIP; q=0.5",
            "*;q=0.2",
            "gzip;q=1, identity;q=0",
        ] {
            assert!(accepts_gzip(Some(header)), "{header}");
        }
        for header in [
            "",
            "br",
            "gzip;q=0",
            "*;q=1,gzip;q=0",
            "gzip;q=NaN",
            "gzip;q=2",
            "gzip;q=bad",
        ] {
            assert!(!accepts_gzip(Some(header)), "{header}");
        }
        assert!(!accepts_gzip(None));
        for header in [
            None,
            Some("gzip"),
            Some("gzip;q=0,*;q=1"),
            Some("*;q=0,identity"),
        ] {
            assert!(accepts_identity(header));
        }
        for header in ["gzip,identity;q=0", "*;q=0", "*;q=1,identity;q=0"] {
            assert!(!accepts_identity(Some(header)));
        }
    }

    #[test]
    fn only_bounded_complete_text_replies_are_compressed_and_round_trip() {
        assert!(!eligible("text/event-stream", 8192));
        assert!(!eligible("application/json", 100));
        assert!(!eligible("application/json", MAX_BYTES + 1));
        assert!(eligible("application/json; charset=utf-8", 8192));
        let body = br#"{"name":"fixture","tracks":[]}"#.repeat(1024);
        let encoded = gzip(&body).unwrap();
        assert!(encoded.len() < body.len() / 10);
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(encoded.as_slice())
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, body);
        assert!(gzip(b"small reply").is_none());
        assert!(gzip(&vec![0; MAX_BYTES + 1]).is_none());
    }
}
