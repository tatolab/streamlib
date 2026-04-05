use crate::coding::{Decode, DecodeError, Encode, EncodeError, KeyValuePairs, Location};

/// Sent by the subscriber to request all future objects for the given track.
///
/// Objects will use the provided ID instead of the full track name, to save bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscribeUpdate {
    /// The request ID of this request
    pub id: u64,

    /// The request ID of the SUBSCRIBE this message is updating.
    pub subscription_request_id: u64,

    /// The starting location
    pub start_location: Location,
    /// The end Group ID, plus 1.  A value of 0 means the subscription is open-ended.
    pub end_group_id: u64,

    /// Subscriber Priority
    pub subscriber_priority: u8,

    /// Forward Flag
    pub forward: bool,

    /// Optional parameters
    pub params: KeyValuePairs,
}

impl Decode for SubscribeUpdate {
    fn decode<R: bytes::Buf>(r: &mut R) -> Result<Self, DecodeError> {
        let id = u64::decode(r)?;

        let subscription_request_id = u64::decode(r)?;

        let start_location = Location::decode(r)?;
        let end_group_id = u64::decode(r)?;

        let subscriber_priority = u8::decode(r)?;

        let forward = bool::decode(r)?;

        let params = KeyValuePairs::decode(r)?;

        Ok(Self {
            id,
            subscription_request_id,
            start_location,
            end_group_id,
            subscriber_priority,
            forward,
            params,
        })
    }
}

impl Encode for SubscribeUpdate {
    fn encode<W: bytes::BufMut>(&self, w: &mut W) -> Result<(), EncodeError> {
        self.id.encode(w)?;

        self.subscription_request_id.encode(w)?;

        self.start_location.encode(w)?;
        self.end_group_id.encode(w)?;

        self.subscriber_priority.encode(w)?;

        self.forward.encode(w)?;

        self.params.encode(w)?;

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

        // One parameter for testing
        let mut kvps = KeyValuePairs::new();
        kvps.set_intvalue(124, 456);

        let msg = SubscribeUpdate {
            id: 1000,
            subscription_request_id: 924,
            start_location: Location::new(1, 1),
            end_group_id: 100000,
            subscriber_priority: 127,
            forward: true,
            params: kvps.clone(),
        };
        msg.encode(&mut buf).unwrap();
        let decoded = SubscribeUpdate::decode(&mut buf).unwrap();
        assert_eq!(decoded, msg);
    }
}
