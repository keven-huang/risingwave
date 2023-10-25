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

use crate::handler::HandlerArgs;

#[derive(Debug, Clone, Default)]
pub struct OverwriteOptions {
    pub stream_rate_control: Option<u32>,
}

impl OverwriteOptions {
    const STREAM_RATE_LIMIT_KEY: &'static str = "streaming_rate_limit";

    pub fn new(args: &mut HandlerArgs) -> Self {
        let stream_rate_control = {
            tracing::info!("with props: {:?}", args.with_options);
            if let Some(x) = args
                .with_options
                .inner_mut()
                .remove(Self::STREAM_RATE_LIMIT_KEY)
            {
                // TODO: validate the value
                Some(x.parse::<u32>().unwrap())
            } else {
                args.session.config().get_streaming_rate_limit()
            }
        };
        Self {
            stream_rate_control,
        }
    }
}
