// SPDX-License-Identifier: MIT
use crate::{Result, Scope};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceRef {
    pub id: String,
    pub mime: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct TimeRange {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl TimeRange {
    pub fn new(start_ms: u64, end_ms: u64) -> Result<Self> {
        if start_ms >= end_ms {
            return Err("time range must have positive duration".into());
        }
        Ok(Self { start_ms, end_ms })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Html {
        html: String,
        truncated: bool,
    },
    Json {
        value: serde_json::Value,
    },
    Image {
        resource: ResourceRef,
        description: Option<String>,
    },
    Audio {
        resource: ResourceRef,
        duration_ms: Option<u64>,
        transcript: Option<String>,
    },
    Video {
        resource: ResourceRef,
        duration_ms: Option<u64>,
        audio: Option<ResourceRef>,
    },
    Source {
        resource: ResourceRef,
        label: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub schema: String,
    pub scope: Scope,
    pub object_id: String,
    pub title: String,
    pub blocks: Vec<ContentBlock>,
}

impl Document {
    pub fn new(scope: Scope, object_id: String, title: String, blocks: Vec<ContentBlock>) -> Self {
        Self {
            schema: "agent-runtime.document.v1".into(),
            scope,
            object_id,
            title,
            blocks,
        }
    }
}

/// Processing coverage describes evidence prepared, never a claim that a model saw every frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub source: ResourceRef,
    pub range: TimeRange,
    pub frame_times_ms: Vec<u64>,
    pub audio_ranges: Vec<TimeRange>,
    pub submitted_to_engine: bool,
    pub gaps: Vec<String>,
}
