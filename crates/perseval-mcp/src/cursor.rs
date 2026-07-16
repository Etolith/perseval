use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CURSOR_VERSION: &str = "perseval.mcp.cursor.v1";

#[derive(Debug, Clone)]
pub(crate) struct CursorCodec {
    workspace_id: String,
    ttl_seconds: u64,
    key: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorPosition {
    pub offset: u64,
    pub commit_sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorError {
    Invalid,
    Expired,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorPayload {
    version: String,
    workspace_id: String,
    tool: String,
    scope_id: Option<String>,
    filter_hash: String,
    offset: String,
    commit_sequence: String,
    expires_at_unix_seconds: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedCursor {
    payload: String,
    signature: String,
}

impl CursorCodec {
    pub(crate) fn new(workspace_id: &str, ttl_seconds: u64) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"perseval-mcp-cursor-key-v1\0");
        hasher.update(workspace_id.as_bytes());
        hasher.update(std::process::id().to_le_bytes());
        hasher.update(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .to_le_bytes(),
        );
        Self {
            workspace_id: workspace_id.to_owned(),
            ttl_seconds,
            key: hasher.finalize().into(),
        }
    }

    pub(crate) fn encode(
        &self,
        tool: &str,
        scope_id: Option<&str>,
        filter_hash: &str,
        offset: u64,
        commit_sequence: u64,
    ) -> Result<String, serde_json::Error> {
        let payload = CursorPayload {
            version: CURSOR_VERSION.into(),
            workspace_id: self.workspace_id.clone(),
            tool: tool.to_owned(),
            scope_id: scope_id.map(str::to_owned),
            filter_hash: filter_hash.to_owned(),
            offset: offset.to_string(),
            commit_sequence: commit_sequence.to_string(),
            expires_at_unix_seconds: now_seconds().saturating_add(self.ttl_seconds).to_string(),
        };
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload)?);
        let signed = SignedCursor {
            signature: self.sign(payload.as_bytes()),
            payload,
        };
        Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&signed)?))
    }

    pub(crate) fn decode(
        &self,
        cursor: &str,
        tool: &str,
        scope_id: Option<&str>,
        filter_hash: &str,
        current_commit_sequence: u64,
    ) -> Result<CursorPosition, CursorError> {
        let signed_bytes = URL_SAFE_NO_PAD
            .decode(cursor.as_bytes())
            .map_err(|_| CursorError::Invalid)?;
        let signed: SignedCursor =
            serde_json::from_slice(&signed_bytes).map_err(|_| CursorError::Invalid)?;
        if !constant_time_eq(
            signed.signature.as_bytes(),
            self.sign(signed.payload.as_bytes()).as_bytes(),
        ) {
            return Err(CursorError::Invalid);
        }
        let payload_bytes = URL_SAFE_NO_PAD
            .decode(signed.payload.as_bytes())
            .map_err(|_| CursorError::Invalid)?;
        let payload: CursorPayload =
            serde_json::from_slice(&payload_bytes).map_err(|_| CursorError::Invalid)?;
        if payload.version != CURSOR_VERSION
            || payload.workspace_id != self.workspace_id
            || payload.tool != tool
            || payload.scope_id.as_deref() != scope_id
            || payload.filter_hash != filter_hash
        {
            return Err(CursorError::Invalid);
        }
        let expires_at = payload
            .expires_at_unix_seconds
            .parse::<u64>()
            .map_err(|_| CursorError::Invalid)?;
        let commit_sequence = payload
            .commit_sequence
            .parse::<u64>()
            .map_err(|_| CursorError::Invalid)?;
        if expires_at < now_seconds() || commit_sequence != current_commit_sequence {
            return Err(CursorError::Expired);
        }
        Ok(CursorPosition {
            offset: payload
                .offset
                .parse::<u64>()
                .map_err(|_| CursorError::Invalid)?,
            commit_sequence,
        })
    }

    fn sign(&self, payload: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"perseval-mcp-cursor-signature-v1\0");
        hasher.update(self.key);
        hasher.update(payload);
        hasher.update(self.key);
        hex::encode(hasher.finalize())
    }
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_binds_tool_scope_filter_and_snapshot() {
        let codec = CursorCodec::new("workspace:test", 900);
        let cursor = codec
            .encode("list_runs", Some("scope:one"), "sha256:filter", 50, 8)
            .unwrap();
        assert_eq!(
            codec
                .decode(&cursor, "list_runs", Some("scope:one"), "sha256:filter", 8,)
                .unwrap(),
            CursorPosition {
                offset: 50,
                commit_sequence: 8,
            }
        );
        assert_eq!(
            codec.decode(
                &cursor,
                "list_projects",
                Some("scope:one"),
                "sha256:filter",
                8,
            ),
            Err(CursorError::Invalid)
        );
        assert_eq!(
            codec.decode(&cursor, "list_runs", Some("scope:one"), "sha256:filter", 9,),
            Err(CursorError::Expired)
        );
    }
}
