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

use anyhow::anyhow;
use itertools::Itertools;
use risingwave_common::catalog::{ColumnDesc, ColumnId, TableId, TableOption};
use risingwave_common::util::sort_util::OrderType;
use risingwave_pb::plan_common::StorageTableDesc;
use risingwave_pb::stream_plan::BatchPlanNode;
use risingwave_storage::table::batch_table::storage_table::StorageTable;
use risingwave_storage::table::Distribution;
use risingwave_storage::StateStore;

use super::*;
use crate::executor::{BatchQueryExecutor, DummyExecutor};

pub struct BatchQueryExecutorBuilder;

#[async_trait::async_trait]
impl ExecutorBuilder for BatchQueryExecutorBuilder {
    type Node = BatchPlanNode;

    async fn new_boxed_executor(
        params: ExecutorParams,
        node: &Self::Node,
        state_store: impl StateStore,
        stream: &mut LocalStreamManagerCore,
    ) -> StreamResult<BoxedExecutor> {
        if node.table_desc.is_none() {
            // used in sharing cdc source backfill as a dummy batch plan node
            return Ok(Box::new(DummyExecutor::new()));
        }

        let table_desc: &StorageTableDesc = node
            .get_table_desc()
            .map_err(|err| anyhow!("batch_plan: table_desc not found! {:?}", err))?;
        let table_id = TableId {
            table_id: table_desc.table_id,
        };

        let order_types = table_desc
            .pk
            .iter()
            .map(|desc| OrderType::from_protobuf(desc.get_order_type().unwrap()))
            .collect_vec();

        let column_descs = table_desc
            .columns
            .iter()
            .map(ColumnDesc::from)
            .collect_vec();
        let column_ids = node
            .column_ids
            .iter()
            .copied()
            .map(ColumnId::from)
            .collect();

        // Use indices based on full table instead of streaming executor output.
        let pk_indices = table_desc
            .pk
            .iter()
            .map(|k| k.column_index as usize)
            .collect_vec();

        let dist_key_in_pk_indices = table_desc
            .dist_key_in_pk_indices
            .iter()
            .map(|&k| k as usize)
            .collect_vec();
        let distribution = match params.vnode_bitmap {
            Some(vnodes) => Distribution {
                dist_key_in_pk_indices,
                vnodes: vnodes.into(),
            },
            None => Distribution::fallback(),
        };

        let table_option = TableOption {
            retention_seconds: if table_desc.retention_seconds > 0 {
                Some(table_desc.retention_seconds)
            } else {
                None
            },
        };
        let value_indices = table_desc
            .get_value_indices()
            .iter()
            .map(|&k| k as usize)
            .collect_vec();
        let prefix_hint_len = table_desc.get_read_prefix_len_hint() as usize;
        let versioned = table_desc.versioned;
        let table = StorageTable::new_partial(
            state_store,
            table_id,
            column_descs,
            column_ids,
            order_types,
            pk_indices,
            distribution,
            table_option,
            value_indices,
            prefix_hint_len,
            versioned,
        );

        let schema = table.schema().clone();
        let executor = BatchQueryExecutor::new(
            table,
            stream.config.developer.chunk_size,
            ExecutorInfo {
                schema,
                pk_indices: params.pk_indices,
                identity: "BatchQuery".to_owned(),
            },
        );

        Ok(executor.boxed())
    }
}
