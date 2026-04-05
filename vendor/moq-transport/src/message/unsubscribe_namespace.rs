use crate::coding::{Decode, DecodeError, Encode, EncodeError, TrackNamespace};

/// Unsubscribe Namespace
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnsubscribeNamespace {
    // Echo back the track namespace prefix from subscribe namespace
    pub track_namespace_prefix: TrackNamespace,
}

impl Decode for UnsubscribeNamespace {
    fn decode<R: bytes::Buf>(r: &mut R) -> Result<Self, DecodeError> {
        let track_namespace_prefix = TrackNamespace::decode(r)?;
        Ok(Self {
            track_namespace_prefix,
        })
    }
}

impl Encode for UnsubscribeNamespace {
    fn encode<W: bytes::BufMut>(&self, w: &mut W) -> Result<(), EncodeError> {
        self.track_namespace_prefix.encode(w)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn encode_decode() {
        let mut buf = BytesMut::new();

        let msg = UnsubscribeNamespace {
            track_namespace_prefix: TrackNamespace::from_utf8_path("test/path/to/resource"),
        };
        msg.encode(&mut buf).unwrap();
        let decoded = UnsubscribeNamespace::decode(&mut buf).unwrap();
        assert_eq!(decoded, msg);
    }
}
