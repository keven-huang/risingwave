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

use std::pin::pin;
use std::sync::Arc;

use either::Either;
use futures::stream::select_with_strategy;
use futures::{pin_mut, stream, StreamExt, TryStreamExt};
use futures_async_stream::try_stream;
use risingwave_common::array::StreamChunk;
use risingwave_common::catalog::Schema;
use risingwave_common::row::OwnedRow;
use risingwave_common::types::Datum;
use risingwave_common::util::epoch::EpochPair;
use risingwave_connector::source::external::BinlogOffset;
use risingwave_storage::StateStore;

use crate::common::table::state_table::StateTable;
use crate::executor::backfill::upstream_table::external::ExternalStorageTable;
use crate::executor::backfill::upstream_table::snapshot::{
    SnapshotReadArgs, UpstreamTableRead, UpstreamTableReader,
};
use crate::executor::backfill::utils;
use crate::executor::backfill::utils::{
    construct_initial_finished_state, get_consumed_binlog_offset, get_new_pos, mapping_chunk,
    mapping_message, mark_cdc_chunk, restore_backfill_progress,
};
use crate::executor::monitor::StreamingMetrics;
use crate::executor::{
    expect_first_barrier, BoxedExecutor, BoxedMessageStream, Executor, ExecutorInfo, Message,
    PkIndices, PkIndicesRef, StreamExecutorError, StreamExecutorResult,
};
use crate::task::{ActorId, CreateMviewProgress};

/// An implementation of the RFC: Use Backfill To Let Mv On Mv Stream Again.(https://github.com/risingwavelabs/rfcs/pull/13)
/// `BackfillExecutor` is used to create a materialized view on another materialized view.
///
/// It can only buffer chunks between two barriers instead of unbundled memory usage of
/// `RearrangedChainExecutor`.
///
/// It uses the latest epoch to read the snapshot of the upstream mv during two barriers and all the
/// `StreamChunk` of the snapshot read will forward to the downstream.
///
/// It uses `current_pos` to record the progress of the backfill (the pk of the upstream mv) and
/// `current_pos` is initiated as an empty `Row`.
///
/// All upstream messages during the two barriers interval will be buffered and decide to forward or
/// ignore based on the `current_pos` at the end of the later barrier. Once `current_pos` reaches
/// the end of the upstream mv pk, the backfill would finish.
///
/// Notice:
/// The pk we are talking about here refers to the storage primary key.
/// We rely on the scheduler to schedule the `BackfillExecutor` together with the upstream mv/table
/// in the same worker, so that we can read uncommitted data from the upstream table without
/// waiting.
/// TODO(siyuan): most code can be reused, I want to implement the cdc backfill logic first then
/// unify the two executor to one
pub struct CdcBackfillExecutor<S: StateStore> {
    /// Upstream table
    upstream_table: ExternalStorageTable,

    /// Upstream changelog with the same schema with the external table.
    upstream: BoxedExecutor,

    /// Internal state table for persisting state of backfill state.
    state_table: Option<StateTable<S>>,

    /// The column indices need to be forwarded to the downstream from the upstream and table scan.
    /// User may select a subset of columns from the upstream table.
    output_indices: Vec<usize>,

    actor_id: ActorId,

    info: ExecutorInfo,

    metrics: Arc<StreamingMetrics>,

    chunk_size: usize,
}

impl<S> CdcBackfillExecutor<S>
where
    S: StateStore,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        upstream_table: ExternalStorageTable,
        upstream: BoxedExecutor,
        state_table: Option<StateTable<S>>,
        output_indices: Vec<usize>,
        _progress: Option<CreateMviewProgress>,
        schema: Schema,
        pk_indices: PkIndices,
        metrics: Arc<StreamingMetrics>,
        chunk_size: usize,
    ) -> Self {
        Self {
            info: ExecutorInfo {
                schema,
                pk_indices,
                identity: "CdcBackfillExecutor".to_owned(),
            },
            upstream_table,
            upstream,
            state_table,
            output_indices,
            actor_id: 0,
            metrics,
            chunk_size,
        }
    }

    #[try_stream(ok = Message, error = StreamExecutorError)]
    async fn execute_inner(mut self) {
        // The primary key columns, in the output columns of the upstream_table scan.
        let pk_in_output_indices = self.upstream_table.pk_in_output_indices().unwrap();
        let state_len = pk_in_output_indices.len() + 2; // +1 for backfill_finished, +1 for vnode key.

        let pk_order = self
            .upstream_table
            .pk_serializer()
            .get_order_types()
            .to_vec();

        let upstream_table_id = self.upstream_table.table_id().table_id;
        let upstream_table_reader = UpstreamTableReader::new(self.upstream_table);

        let mut upstream = self.upstream.execute();

        // Current position of the upstream_table storage primary key.
        // `None` means it starts from the beginning.
        let mut current_pk_pos: Option<OwnedRow> = None;

        // Poll the upstream to get the first barrier.
        let first_barrier = expect_first_barrier(&mut upstream).await?;
        let init_epoch = first_barrier.epoch.prev;

        if let Some(state_table) = self.state_table.as_mut() {
            state_table.init_epoch(first_barrier.epoch);
        }

        // TODO(siyuan): restore backfill offset from persistent state
        let (backfill_offset, is_finished) = if let Some(state_table) = self.state_table.as_mut() {
            restore_backfill_progress(state_table, state_len).await?
        } else {
            (None, false)
        };

        current_pk_pos = backfill_offset;

        // If the snapshot is empty, we don't need to backfill.
        // We cannot complete progress now, as we want to persist
        // finished state to state store first.
        // As such we will wait for next barrier.
        let is_snapshot_empty: bool = {
            if is_finished {
                // It is finished, so just assign a value to avoid accessing storage table again.
                false
            } else {
                let args = SnapshotReadArgs::new(init_epoch, None, false, self.chunk_size);
                let snapshot = upstream_table_reader.snapshot_read(args);
                pin_mut!(snapshot);
                snapshot.try_next().await?.unwrap().is_none()
            }
        };

        // | backfill_is_finished | snapshot_empty | need_to_backfill |
        // | t                    | t/f            | f                |
        // | f                    | t              | f                |
        // | f                    | f              | t                |
        let to_backfill = !is_finished && !is_snapshot_empty;

        // Use these to persist state.
        // They contain the backfill position,
        // as well as the progress.
        // However, they do not contain the vnode key at index 0.
        // That is filled in when we flush the state table.
        let mut current_state: Vec<Datum> = vec![None; state_len];
        let mut old_state: Option<Vec<Datum>> = None;

        // The first barrier message should be propagated.
        yield Message::Barrier(first_barrier);

        // If no need backfill, but state was still "unfinished" we need to finish it.
        // So we just update the state + progress to meta at the next barrier to finish progress,
        // and forward other messages.
        //
        // Reason for persisting on second barrier rather than first:
        // We can't update meta with progress as finished until state_table
        // has been updated.
        // We also can't update state_table in first epoch, since state_table
        // expects to have been initialized in previous epoch.

        // Keep track of rows from the snapshot.
        let mut total_snapshot_processed_rows: u64 = 0;

        let mut last_binlog_offset: Option<BinlogOffset>;

        let mut consumed_binlog_offset: Option<BinlogOffset> = None;

        // Backfill Algorithm:
        //
        //   backfill_stream
        //  /               \
        // upstream       snapshot
        //
        // We construct a backfill stream with upstream as its left input and mv snapshot read
        // stream as its right input. When a chunk comes from upstream, we will buffer it.
        //
        // When the first barrier comes from upstream:
        //  - read the current binlog offset as `binlog_low`
        //  - start a snapshot read upon upstream table and iterate over the snapshot read stream
        //  - buffer the changelog event from upstream
        //
        // When a new barrier comes from upstream:
        //  - read the current binlog offset as `binlog_high`
        //  - for each row of the upstream change log, forward it to downstream if it in the range
        //    of [binlog_low, binlog_high] and its pk <= `current_pos`, otherwise ignore it
        //  - reconstruct the whole backfill stream with upstream changelog and a new table snapshot
        //
        // When a chunk comes from snapshot, we forward it to the downstream and raise
        // `current_pos`.
        // When we reach the end of the snapshot read stream, it means backfill has been
        // finished.
        //
        // Once the backfill loop ends, we forward the upstream directly to the downstream.
        if to_backfill {
            // init the last binlog offset
            last_binlog_offset = upstream_table_reader.current_binlog_offset().await?;

            'backfill_loop: loop {
                let mut upstream_chunk_buffer: Vec<StreamChunk> = vec![];

                let left_upstream = upstream.by_ref().map(Either::Left);

                let args = SnapshotReadArgs::new_for_cdc(current_pk_pos.clone(), self.chunk_size);
                let right_snapshot =
                    pin!(upstream_table_reader.snapshot_read(args).map(Either::Right));

                // Prefer to select upstream, so we can stop snapshot stream when barrier comes.
                let backfill_stream =
                    select_with_strategy(left_upstream, right_snapshot, |_: &mut ()| {
                        stream::PollNext::Left
                    });

                let mut cur_barrier_snapshot_processed_rows: u64 = 0;
                let mut cur_barrier_upstream_processed_rows: u64 = 0;

                #[for_await]
                for either in backfill_stream {
                    match either {
                        // Upstream
                        Either::Left(msg) => {
                            match msg? {
                                Message::Barrier(barrier) => {
                                    // If it is a barrier, switch snapshot and consume
                                    // upstream buffer chunk
                                    // Consume upstream buffer chunk
                                    // If no current_pos, means we did not process any snapshot yet.
                                    // In that case we can just ignore the upstream buffer chunk.
                                    if let Some(current_pos) = &current_pk_pos {
                                        for chunk in upstream_chunk_buffer.drain(..) {
                                            cur_barrier_upstream_processed_rows +=
                                                chunk.cardinality() as u64;

                                            // record the consumed binlog offset which will be
                                            // persisted later
                                            consumed_binlog_offset = get_consumed_binlog_offset(
                                                upstream_table_reader.inner().table_reader(),
                                                &chunk,
                                            )?;
                                            yield Message::Chunk(mapping_chunk(
                                                mark_cdc_chunk(
                                                    upstream_table_reader.inner().table_reader(),
                                                    chunk,
                                                    current_pos,
                                                    &pk_in_output_indices,
                                                    &pk_order,
                                                    last_binlog_offset.clone(),
                                                )?,
                                                &self.output_indices,
                                            ));
                                        }
                                    }

                                    self.metrics
                                        .backfill_snapshot_read_row_count
                                        .with_label_values(&[
                                            upstream_table_id.to_string().as_str(),
                                            self.actor_id.to_string().as_str(),
                                        ])
                                        .inc_by(cur_barrier_snapshot_processed_rows);

                                    self.metrics
                                        .backfill_upstream_output_row_count
                                        .with_label_values(&[
                                            upstream_table_id.to_string().as_str(),
                                            self.actor_id.to_string().as_str(),
                                        ])
                                        .inc_by(cur_barrier_upstream_processed_rows);

                                    // Update last seen binlog offset
                                    last_binlog_offset = consumed_binlog_offset.clone();

                                    // Persist state on barrier
                                    // TODO(siyuan): persist the `last_binlog_offset` wihch will
                                    // be a starting point upon recovery
                                    Self::persist_state(
                                        barrier.epoch,
                                        &mut self.state_table,
                                        false,
                                        &current_pk_pos,
                                        &mut old_state,
                                        &mut current_state,
                                    )
                                    .await?;

                                    yield Message::Barrier(barrier);
                                    // Break the for loop and start a new snapshot read stream.
                                    break;
                                }
                                Message::Chunk(chunk) => {
                                    // Buffer the upstream chunk.
                                    upstream_chunk_buffer.push(chunk.compact());
                                }
                                Message::Watermark(_) => {
                                    // Ignore watermark during backfill.
                                }
                            }
                        }
                        // Snapshot read
                        Either::Right(msg) => {
                            match msg? {
                                None => {
                                    // TODO(siyuan): handle the case when snapshot read stream ends.
                                    // End of the snapshot read stream.
                                    // We should not mark the chunk anymore,
                                    // otherwise, we will ignore some rows
                                    // in the buffer. Here we choose to never mark the chunk.
                                    // Consume with the renaming stream buffer chunk without mark.
                                    for chunk in upstream_chunk_buffer.drain(..) {
                                        let chunk_cardinality = chunk.cardinality() as u64;
                                        cur_barrier_snapshot_processed_rows += chunk_cardinality;
                                        total_snapshot_processed_rows += chunk_cardinality;
                                        yield Message::Chunk(mapping_chunk(
                                            chunk,
                                            &self.output_indices,
                                        ));
                                    }

                                    break 'backfill_loop;
                                }
                                Some(chunk) => {
                                    // Raise the current position.
                                    // As snapshot read streams are ordered by pk, so we can
                                    // just use the last row to update `current_pos`.
                                    current_pk_pos =
                                        Some(get_new_pos(&chunk, &pk_in_output_indices));

                                    tracing::info!("snapshot chunk: {:#?}", chunk);
                                    let chunk_cardinality = chunk.cardinality() as u64;
                                    cur_barrier_snapshot_processed_rows += chunk_cardinality;
                                    total_snapshot_processed_rows += chunk_cardinality;
                                    yield Message::Chunk(mapping_chunk(
                                        chunk,
                                        &self.output_indices,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        tracing::trace!(
            actor = self.actor_id,
            "Backfill has already finished and forward messages directly to the downstream"
        );

        // Wait for first barrier to come after backfill is finished.
        // So we can update our progress + persist the status.
        while let Some(Ok(msg)) = upstream.next().await {
            if let Some(msg) = mapping_message(msg, &self.output_indices) {
                // If not finished then we need to update state, otherwise no need.
                if let Message::Barrier(barrier) = &msg && !is_finished {
                    // If snapshot was empty, we do not need to backfill,
                    // but we still need to persist the finished state.
                    // We currently persist it on the second barrier here rather than first.
                    // This is because we can't update state table in first epoch,
                    // since it expects to have been initialized in previous epoch
                    // (there's no epoch before the first epoch).
                    if is_snapshot_empty {
                        current_pk_pos =
                            Some(construct_initial_finished_state(pk_in_output_indices.len()))
                    }

                    // We will update current_pos at least once,
                    // since snapshot read has to be non-empty,
                    // Or snapshot was empty and we construct a placeholder state.
                    debug_assert_ne!(current_pk_pos, None);

                    Self::persist_state(
                        barrier.epoch,
                        &mut self.state_table,
                        true,
                        &current_pk_pos,
                        &mut old_state,
                        &mut current_state,
                    )
                    .await?;
                    yield msg;
                    break;
                }
                yield msg;
            }
        }

        // After progress finished + state persisted,
        // we can forward messages directly to the downstream,
        // as backfill is finished.
        #[for_await]
        for msg in upstream {
            if let Some(msg) = mapping_message(msg?, &self.output_indices) {
                if let Some(state_table) = self.state_table.as_mut() && let Message::Barrier(barrier) = &msg {
                        state_table.commit_no_data_expected(barrier.epoch);
                    }
                yield msg;
            }
        }
    }

    async fn persist_state(
        epoch: EpochPair,
        table: &mut Option<StateTable<S>>,
        is_finished: bool,
        current_pos: &Option<OwnedRow>,
        old_state: &mut Option<Vec<Datum>>,
        current_state: &mut [Datum],
    ) -> StreamExecutorResult<()> {
        // Backwards compatibility with no state table in backfill.
        let Some(table) = table else {
            return Ok(())
        };
        utils::persist_state(
            epoch,
            table,
            is_finished,
            current_pos,
            old_state,
            current_state,
        )
        .await
    }
}

impl<S> Executor for CdcBackfillExecutor<S>
where
    S: StateStore,
{
    fn execute(self: Box<Self>) -> BoxedMessageStream {
        self.execute_inner().boxed()
    }

    fn schema(&self) -> &Schema {
        &self.info.schema
    }

    fn pk_indices(&self) -> PkIndicesRef<'_> {
        &self.info.pk_indices
    }

    fn identity(&self) -> &str {
        &self.info.identity
    }
}