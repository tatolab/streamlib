use crate::coding::{Decode, DecodeError, Encode, EncodeError, KeyValuePairs, Location};
use crate::message::FilterType;
use crate::message::GroupOrder;

/// Sent by the subscriber to request all future objects for the given track.
///
/// Objects will use the provided ID instead of the full track name, to save bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishOk {
    /// The request ID of the Publish this message is replying to.
    pub id: u64,

    /// Forward Flag
    pub forward: bool,

    /// Subscriber Priority
    pub subscriber_priority: u8,

    /// The order the subscription will be delivered in
    pub group_order: GroupOrder,

    /// Filter type
    pub filter_type: FilterType,

    /// The starting location for this subscription. Only present for "AbsoluteStart" and "AbsoluteRange" filter types.
    pub start_location: Option<Location>,
    /// End group id, inclusive, for the subscription, if applicable. Only present for "AbsoluteRange" filter type.
    pub end_group_id: Option<u64>,

    /// Optional parameters
    pub params: KeyValuePairs,
}

impl Decode for PublishOk {
    fn decode<R: bytes::Buf>(r: &mut R) -> Result<Self, DecodeError> {
        let id = u64::decode(r)?;

        let forward = bool::decode(r)?;
        let subscriber_priority = u8::decode(r)?;
        let group_order = GroupOrder::decode(r)?;

        let filter_type = FilterType::decode(r)?;
        let start_location: Option<Location>;
        let end_group_id: Option<u64>;
        match filter_type {
            FilterType::AbsoluteStart => {
                start_location = Some(Location::decode(r)?);
                end_group_id = None;
            }
            FilterType::AbsoluteRange => {
                start_location = Some(Location::decode(r)?);
                end_group_id = Some(u64::decode(r)?);
            }
            _ => {
                start_location = None;
                end_group_id = None;
            }
        }

        let params = KeyValuePairs::decode(r)?;

        Ok(Self {
            id,
            forward,
            subscriber_priority,
            group_order,
            filter_type,
            start_location,
            end_group_id,
            params,
        })
    }
}

impl Encode for PublishOk {
    fn encode<W: bytes::BufMut>(&self, w: &mut W) -> Result<(), EncodeError> {
        self.id.encode(w)?;

        self.forward.encode(w)?;
        self.subscriber_priority.encode(w)?;
        self.group_order.encode(w)?;

        self.filter_type.encode(w)?;
        match self.filter_type {
            FilterType::AbsoluteStart => {
                if let Some(start) = &self.start_location {
                    start.encode(w)?;
                } else {
                    return Err(EncodeError::MissingField("StartLocation".to_string()));
                }
                // Just ignore end_group_id if it happens to be set
            }
            FilterType::AbsoluteRange => {
                if let Some(start) = &self.start_location {
                    start.encode(w)?;
                } else {
                    return Err(EncodeError::MissingField("StartLocation".to_string()));
                }
                if let Some(end) = self.end_group_id {
                    end.encode(w)?;
                } else {
                    return Err(EncodeError::MissingField("EndGroupId".to_string()));
                }
            }
            _ => {}
        }

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
        kvps.set_bytesvalue(123, vec![0x00, 0x01, 0x02, 0x03]);

        // FilterType = NextGroupStart
        let msg = PublishOk {
            id: 12345,
            forward: true,
            subscriber_priority: 127,
            group_order: GroupOrder::Publisher,
            filter_type: FilterType::NextGroupStart,
            start_location: None,
            end_group_id: None,
            params: kvps.clone(),
        };
        msg.encode(&mut buf).unwrap();
        let decoded = PublishOk::decode(&mut buf).unwrap();
        assert_eq!(decoded, msg);

        // FilterType = AbsoluteStart
        let msg = PublishOk {
            id: 12345,
            forward: true,
            subscriber_priority: 127,
            group_order: GroupOrder::Publisher,
            filter_type: FilterType::AbsoluteStart,
            start_location: Some(Location::new(12345, 67890)),
            end_group_id: None,
            params: kvps.clone(),
        };
        msg.encode(&mut buf).unwrap();
        let decoded = PublishOk::decode(&mut buf).unwrap();
        assert_eq!(decoded, msg);

        // FilterType = AbsoluteRange
        let msg = PublishOk {
            id: 12345,
            forward: true,
            subscriber_priority: 127,
            group_order: GroupOrder::Publisher,
            filter_type: FilterType::AbsoluteRange,
            start_location: Some(Location::new(12345, 67890)),
            end_group_id: Some(23456),
            params: kvps.clone(),
        };
        msg.encode(&mut buf).unwrap();
        let decoded = PublishOk::decode(&mut buf).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn encode_missing_fields() {
        let mut buf = BytesMut::new();

        // FilterType = AbsoluteStart - missing start_location
        let msg = PublishOk {
            id: 12345,
            forward: true,
            subscriber_priority: 127,
            group_order: GroupOrder::Publisher,
            filter_type: FilterType::AbsoluteStart,
            start_location: None,
            end_group_id: None,
            params: Default::default(),
        };
        let encoded = msg.encode(&mut buf);
        assert!(matches!(encoded.unwrap_err(), EncodeError::MissingField(_)));

        // FilterType = AbsoluteRange - missing start_location
        let msg = PublishOk {
            id: 12345,
            forward: true,
            subscriber_priority: 127,
            group_order: GroupOrder::Publisher,
            filter_type: FilterType::AbsoluteRange,
            start_location: None,
            end_group_id: None,
            params: Default::default(),
        };
        let encoded = msg.encode(&mut buf);
        assert!(matches!(encoded.unwrap_err(), EncodeError::MissingField(_)));

        // FilterType = AbsoluteRange - missing end_group_id
        let msg = PublishOk {
            id: 12345,
            forward: true,
            subscriber_priority: 127,
            group_order: GroupOrder::Publisher,
            filter_type: FilterType::AbsoluteRange,
            start_location: Some(Location::new(12345, 67890)),
            end_group_id: None,
            params: Default::default(),
        };
        let encoded = msg.encode(&mut buf);
        assert!(matches!(encoded.unwrap_err(), EncodeError::MissingField(_)));
    }
}
