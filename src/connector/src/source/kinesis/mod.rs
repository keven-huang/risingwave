// Copyright 2023 RisingWave Labs
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

pub mod enumerator;
pub mod source;
pub mod split;

use serde::Deserialize;
use with_options::WithOptions;

use crate::common::KinesisCommon;
use crate::source::kinesis::enumerator::client::KinesisSplitEnumerator;
use crate::source::kinesis::source::reader::KinesisSplitReader;
use crate::source::kinesis::split::KinesisSplit;
use crate::source::SourceProperties;

pub const KINESIS_CONNECTOR: &str = "kinesis";

#[derive(Clone, Debug, Deserialize, WithOptions)]
pub struct KinesisProperties {
    #[serde(rename = "scan.startup.mode", alias = "kinesis.scan.startup.mode")]
    // accepted values: "latest", "earliest", "timestamp"
    pub scan_startup_mode: Option<String>,

    #[serde(rename = "scan.startup.timestamp.millis")]
    pub timestamp_offset: Option<i64>,

    #[serde(flatten)]
    pub common: KinesisCommon,
}

impl SourceProperties for KinesisProperties {
    type Split = KinesisSplit;
    type SplitEnumerator = KinesisSplitEnumerator;
    type SplitReader = KinesisSplitReader;

    const SOURCE_NAME: &'static str = KINESIS_CONNECTOR;
}
