use bytes::Bytes;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    TransferRequest(TransferRequest),
    TransferResponse(TransferResponse),
    ChunkData(ChunkData),
    ChunkAck(ChunkAck),
    TransferComplete(TransferComplete),
    TransferError(TransferError),
    AuthRequest(AuthRequest),
    AuthResponse(AuthResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferRequest {
    pub file_id: String,
    pub file_name: String,
    pub relative_path: Option<String>,
    pub file_size: u64,
    pub chunk_size: u32,
    pub device_id: String,
    pub password_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferResponse {
    pub file_id: String,
    pub accepted: bool,
    pub save_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ChunkData {
    pub file_id: String,
    pub chunk_index: u32,
    pub data: Bytes,
    pub checksum: u32,
    pub compressed: bool,
}

impl Serialize for ChunkData {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("ChunkData", 5)?;
        state.serialize_field("file_id", &self.file_id)?;
        state.serialize_field("chunk_index", &self.chunk_index)?;
        state.serialize_field("data", &self.data.as_ref())?;
        state.serialize_field("checksum", &self.checksum)?;
        state.serialize_field("compressed", &self.compressed)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for ChunkData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ChunkDataHelper {
            file_id: String,
            chunk_index: u32,
            data: Vec<u8>,
            checksum: u32,
            compressed: bool,
        }

        let helper = ChunkDataHelper::deserialize(deserializer)?;
        Ok(ChunkData {
            file_id: helper.file_id,
            chunk_index: helper.chunk_index,
            data: Bytes::from(helper.data),
            checksum: helper.checksum,
            compressed: helper.compressed,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkAck {
    pub file_id: String,
    pub chunk_index: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferComplete {
    pub file_id: String,
    pub file_hash: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferError {
    pub file_id: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthRequest {
    pub device_id: String,
    pub password_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResponse {
    pub accepted: bool,
    pub reason: Option<String>,
}

impl Message {
    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        Ok(bincode::serialize(self)?)
    }

    pub fn from_bytes(data: &[u8]) -> anyhow::Result<Self> {
        Ok(bincode::deserialize(data)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transfer_request_serialization() {
        let request = TransferRequest {
            file_id: "test-file-id".to_string(),
            file_name: "test.txt".to_string(),
            relative_path: Some("subdir/test.txt".to_string()),
            file_size: 1024,
            chunk_size: 1048576,
            device_id: "device-123".to_string(),
            password_hash: Some("hash123".to_string()),
        };

        let serialized = bincode::serialize(&request).unwrap();
        let deserialized: TransferRequest = bincode::deserialize(&serialized).unwrap();

        assert_eq!(request.file_id, deserialized.file_id);
        assert_eq!(request.file_name, deserialized.file_name);
        assert_eq!(request.relative_path, deserialized.relative_path);
        assert_eq!(request.file_size, deserialized.file_size);
        assert_eq!(request.chunk_size, deserialized.chunk_size);
    }

    #[test]
    fn test_chunk_data_serialization() {
        let chunk = ChunkData {
            file_id: "test-file-id".to_string(),
            chunk_index: 5,
            data: Bytes::from(vec![1, 2, 3, 4, 5]),
            checksum: 12345,
            compressed: true,
        };

        let serialized = bincode::serialize(&chunk).unwrap();
        let deserialized: ChunkData = bincode::deserialize(&serialized).unwrap();

        assert_eq!(chunk.file_id, deserialized.file_id);
        assert_eq!(chunk.chunk_index, deserialized.chunk_index);
        assert_eq!(chunk.data.as_ref(), deserialized.data.as_ref());
        assert_eq!(chunk.checksum, deserialized.checksum);
        assert_eq!(chunk.compressed, deserialized.compressed);
    }

    #[test]
    fn test_message_to_from_bytes() {
        let request = TransferRequest {
            file_id: "test".to_string(),
            file_name: "file.txt".to_string(),
            relative_path: None,
            file_size: 100,
            chunk_size: 1024,
            device_id: "dev1".to_string(),
            password_hash: None,
        };

        let message = Message::TransferRequest(request.clone());
        let bytes = message.to_bytes().unwrap();
        let decoded = Message::from_bytes(&bytes).unwrap();

        if let Message::TransferRequest(decoded_req) = decoded {
            assert_eq!(request.file_id, decoded_req.file_id);
            assert_eq!(request.file_name, decoded_req.file_name);
        } else {
            panic!("Expected TransferRequest");
        }
    }
}
