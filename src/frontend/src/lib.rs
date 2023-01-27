// Copyright 2023 Singularity Data
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

#![allow(clippy::derive_partial_eq_without_eq)]
#![allow(rustdoc::private_intra_doc_links)]
#![feature(map_try_insert)]
#![feature(negative_impls)]
#![feature(generators)]
#![feature(proc_macro_hygiene, stmt_expr_attributes)]
#![feature(trait_alias)]
#![feature(drain_filter)]
#![feature(if_let_guard)]
#![feature(let_chains)]
#![feature(assert_matches)]
#![feature(lint_reasons)]
#![feature(box_patterns)]
#![feature(once_cell)]
#![feature(result_option_inspect)]
#![feature(macro_metavar_expr)]
#![recursion_limit = "256"]

#[macro_use]
mod catalog;
pub use catalog::TableCatalog;
mod binder;
pub use binder::{bind_data_type, Binder};
pub mod expr;
pub mod handler;
pub use handler::PgResponseStream;
mod observer;
mod optimizer;
pub use optimizer::{OptimizerContext, OptimizerContextRef, PlanRef};
mod planner;
pub use planner::Planner;
#[expect(dead_code)]
mod scheduler;
pub mod session;
mod stream_fragmenter;
use risingwave_common::config::{load_config, FrontendConfig};
pub use stream_fragmenter::build_graph;
mod utils;
pub use utils::{explain_stream_graph, WithOptions};
mod meta_client;
pub mod test_utils;
mod user;

pub mod health_service;
mod monitor;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use pgwire::pg_protocol::TlsConfig;
use pgwire::pg_server::pg_serve;
use session::SessionManagerImpl;

/// Start frontend
pub fn start(opts: FrontendConfig) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    // WARNING: don't change the function signature. Making it `async fn` will cause
    // slow compile in release mode.
    Box::pin(async move {
        let config = load_config(&opts.config_path.clone(), Some(opts));
        let listen_addr = config.frontend.listen_addr.clone();
        let session_mgr = Arc::new(SessionManagerImpl::new(config).await.unwrap());
        pg_serve(&listen_addr, session_mgr, Some(TlsConfig::new_default()))
            .await
            .unwrap();
    })
}
