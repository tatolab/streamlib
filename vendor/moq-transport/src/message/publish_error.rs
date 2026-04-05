use crate::coding::{Decode, DecodeError, Encode, EncodeError, ReasonPhrase};

// TODO SLG - The next draft is going to merge all these error messages to a
//            common RequestError message, so we won't do a lot of work on these
//            existing messages.  We should add an enum for all the various error codes.

/// Sent by the subscriber to reject an Announce.
#[derive(Clone, Debug)]
pub struct PublishError {
    pub id: u64,

    // An error code.
    pub error_code: u64,

    // An optional, human-readable reason.
    pub reason_phrase: ReasonPhrase,
}

impl Decode for PublishError {
    fn decode<R: bytes::Buf>(r: &mut R) -> Result<Self, DecodeError> {
        let id = u64::decode(r)?;
        let error_code = u64::decode(r)?;
        let reason_phrase = ReasonPhrase::decode(r)?;

        Ok(Self {
            id,
            error_code,
            reason_phrase,
        })
    }
}

impl Encode for PublishError {
    fn encode<W: bytes::BufMut>(&self, w: &mut W) -> Result<(), EncodeError> {
        self.id.encode(w)?;
        self.error_code.encode(w)?;
        self.reason_phrase.encode(w)?;

        Ok(())
    }
}
