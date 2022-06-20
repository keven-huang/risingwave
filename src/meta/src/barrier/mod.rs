// Copyright 2022 Singularity Data
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::VecDeque;
use std::iter::once;
use std::sync::Arc;
use std::time::Duration;

use fail::fail_point;
use futures::future::try_join_all;
use itertools::Itertools;
use prometheus::HistogramTimer;
use risingwave_common::catalog::TableId;
use risingwave_common::error::{ErrorCode, Result, RwError};
use risingwave_common::util::epoch::INVALID_EPOCH;
use risingwave_hummock_sdk::{HummockEpoch, LocalSstableInfo};
use risingwave_pb::common::worker_node::State::Running;
use risingwave_pb::common::WorkerType;
use risingwave_pb::data::Barrier;
use risingwave_pb::stream_service::{
    BarrierCompleteRequest, BarrierCompleteResponse, InjectBarrierRequest,
};
use smallvec::SmallVec;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot::{Receiver, Sender};
use tokio::sync::{oneshot, watch, RwLock};
use tokio::task::JoinHandle;
use uuid::Uuid;

pub use self::command::Command;
use self::command::CommandContext;
use self::info::BarrierActorInfo;
use self::notifier::Notifier;
use crate::barrier::progress::CreateMviewProgressTracker;
use crate::barrier::BarrierEpochState::{Complete, Fail, InFlight};
use crate::cluster::{ClusterManagerRef, META_NODE_ID};
use crate::hummock::HummockManagerRef;
use crate::manager::{CatalogManagerRef, MetaSrvEnv};
use crate::model::BarrierManagerState;
use crate::rpc::metrics::MetaMetrics;
use crate::storage::MetaStore;
use crate::stream::FragmentManagerRef;

mod command;
mod info;
mod notifier;
mod progress;
mod recovery;

type Scheduled = (Command, SmallVec<[Notifier; 1]>);

/// A buffer or queue for scheduling barriers.
struct ScheduledBarriers {
    buffer: RwLock<VecDeque<Scheduled>>,

    /// When `buffer` is not empty anymore, all subscribers of this watcher will be notified.
    changed_tx: watch::Sender<()>,
}

impl ScheduledBarriers {
    fn new() -> Self {
        Self {
            buffer: RwLock::new(VecDeque::new()),
            changed_tx: watch::channel(()).0,
        }
    }

    /// Pop a scheduled barrier from the buffer, or a default checkpoint barrier if not exists.
    async fn pop_or_default(&self) -> Scheduled {
        let mut buffer = self.buffer.write().await;

        // If no command scheduled, create periodic checkpoint barrier by default.
        buffer
            .pop_front()
            .unwrap_or_else(|| (Command::checkpoint(), Default::default()))
    }

    /// Wait for at least one scheduled barrier in the buffer.
    async fn wait_one(&self) {
        let buffer = self.buffer.read().await;
        if buffer.len() > 0 {
            return;
        }
        let mut rx = self.changed_tx.subscribe();
        drop(buffer);

        rx.changed().await.unwrap();
    }

    /// Push a scheduled barrier into the buffer.
    async fn push(&self, scheduled: Scheduled) {
        let mut buffer = self.buffer.write().await;
        buffer.push_back(scheduled);
        if buffer.len() == 1 {
            self.changed_tx.send(()).ok();
        }
    }

    /// Attach `new_notifiers` to the very first scheduled barrier. If there's no one scheduled, a
    /// default checkpoint barrier will be created.
    async fn attach_notifiers(&self, new_notifiers: impl IntoIterator<Item = Notifier>) {
        let mut buffer = self.buffer.write().await;
        match buffer.front_mut() {
            Some((_, notifiers)) => notifiers.extend(new_notifiers),
            None => {
                // If no command scheduled, create periodic checkpoint barrier by default.
                buffer.push_back((Command::checkpoint(), new_notifiers.into_iter().collect()));
                if buffer.len() == 1 {
                    self.changed_tx.send(()).ok();
                }
            }
        }
    }

    /// Clear all buffered scheduled barriers, and notify their subscribers with failed as aborted.
    async fn abort(&self) {
        let mut buffer = self.buffer.write().await;
        while let Some((_, notifiers)) = buffer.pop_front() {
            notifiers.into_iter().for_each(|notify| {
                notify.notify_collection_failed(RwError::from(ErrorCode::InternalError(
                    "Scheduled barrier abort.".to_string(),
                )))
            })
        }
    }
}

/// [`crate::barrier::GlobalBarrierManager`] sends barriers to all registered compute nodes and
/// collect them, with monotonic increasing epoch numbers. On compute nodes, `LocalBarrierManager`
/// in `risingwave_stream` crate will serve these requests and dispatch them to source actors.
///
/// Configuration change in our system is achieved by the mutation in the barrier. Thus,
/// [`crate::barrier::GlobalBarrierManager`] provides a set of interfaces like a state machine,
/// accepting [`Command`] that carries info to build `Mutation`. To keep the consistency between
/// barrier manager and meta store, some actions like "drop materialized view" or "create mv on mv"
/// must be done in barrier manager transactional using [`Command`].
pub struct GlobalBarrierManager<S: MetaStore> {
    /// The maximal interval for sending a barrier.
    interval: Duration,

    /// Enable recovery or not when failover.
    enable_recovery: bool,

    /// The queue of scheduled barriers.
    scheduled_barriers: ScheduledBarriers,

    cluster_manager: ClusterManagerRef<S>,

    catalog_manager: CatalogManagerRef<S>,

    fragment_manager: FragmentManagerRef<S>,

    hummock_manager: HummockManagerRef<S>,

    metrics: Arc<MetaMetrics>,

    env: MetaSrvEnv<S>,

    /// The max barrier nums in flight
    in_flight_barrier_nums: usize,
}

struct ConcurrentControl<S: MetaStore> {
    /// Signal whether to start recovery
    is_recovery: bool,
    /// Signal whether the barrier with building actor is sent
    is_build_actor: bool,
    /// Save the states and messages of barrier in order
    command_ctx_queue: VecDeque<EpochNode<S>>,
}
impl<S> ConcurrentControl<S>
where
    S: MetaStore,
{
    fn new() -> Self {
        Self {
            is_recovery: false,
            is_build_actor: false,
            command_ctx_queue: VecDeque::default(),
        }
    }

    /// Pause inject barrier until True
    fn can_inject_barrier(&self, in_flight_barrier_nums: usize) -> bool {
        !(self.is_recovery
            || self.is_build_actor
            || self
                .command_ctx_queue
                .iter()
                .filter(|x| matches!(x.states, InFlight))
                .count()
                >= in_flight_barrier_nums)
    }

    // Push a new item with InFlight state into the command_ctx_queue.
    fn inject_barrier(&mut self, command_ctx: CommandContext<S>, notifier: SmallVec<[Notifier; 1]>, timer: EpochNode) {

    }

    fn succeed_barrier(&mut self, )
}

/// The states and messages of this barrier
struct EpochNode<S: MetaStore> {
    timer: Option<HistogramTimer>,
    result: Option<Result<Vec<BarrierCompleteResponse>>>,
    states: BarrierEpochState,
    command_ctx: Arc<CommandContext<S>>,
    notifiers: SmallVec<[Notifier; 1]>,
}
/// The states of barrier
#[derive(PartialEq)]
enum BarrierEpochState {
    InFlight,
    Complete,
    Fail(RwError),
}

impl<S> GlobalBarrierManager<S>
where
    S: MetaStore,
{
    /// Create a new [`crate::barrier::GlobalBarrierManager`].
    pub fn new(
        env: MetaSrvEnv<S>,
        cluster_manager: ClusterManagerRef<S>,
        catalog_manager: CatalogManagerRef<S>,
        fragment_manager: FragmentManagerRef<S>,
        hummock_manager: HummockManagerRef<S>,
        metrics: Arc<MetaMetrics>,
    ) -> Self {
        let enable_recovery = env.opts.enable_recovery;
        let interval = env.opts.checkpoint_interval;
        let in_flight_barrier_nums = env.opts.in_flight_barrier_nums;
        tracing::info!(
            "Starting barrier manager with: interval={:?}, enable_recovery={} , in_flight_barrier_nums={}",
            interval,
            enable_recovery,
            in_flight_barrier_nums,
        );

        Self {
            interval,
            enable_recovery,
            cluster_manager,
            catalog_manager,
            fragment_manager,
            scheduled_barriers: ScheduledBarriers::new(),
            hummock_manager,
            metrics,
            env,
            in_flight_barrier_nums,
        }
    }

    pub async fn start(barrier_manager: BarrierManagerRef<S>) -> (JoinHandle<()>, Sender<()>) {
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let join_handle = tokio::spawn(async move {
            barrier_manager.run(shutdown_rx).await;
        });

        (join_handle, shutdown_tx)
    }

    /// Start an infinite loop to take scheduled barriers and send them.
    async fn run(&self, mut shutdown_rx: Receiver<()>) {
        let mut tracker = CreateMviewProgressTracker::default();
        let mut state = BarrierManagerState::create(self.env.meta_store()).await;
        if self.enable_recovery {
            // handle init, here we simply trigger a recovery process to achieve the consistency. We
            // may need to avoid this when we have more state persisted in meta store.
            let new_epoch = state.in_flight_prev_epoch.next();
            assert!(new_epoch > state.in_flight_prev_epoch);
            state.in_flight_prev_epoch = new_epoch;

            let (new_epoch, actors_to_track, create_mview_progress) =
                self.recovery(state.in_flight_prev_epoch).await;
            tracker.add(new_epoch, actors_to_track, vec![]);
            for progress in create_mview_progress {
                tracker.update(progress);
            }
            state.in_flight_prev_epoch = new_epoch;
            state
                .update_inflight_prev_epoch(self.env.meta_store())
                .await
                .unwrap();
        }
        let mut min_interval = tokio::time::interval(self.interval);
        min_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut barrier_timer: Option<HistogramTimer> = None;
        let (barrier_complete_tx, mut barrier_complete_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut concurrent_control = ConcurrentControl::new();
        loop {
            tokio::select! {
                biased;
                // Shutdown
                _ = &mut shutdown_rx => {
                    tracing::info!("Barrier manager inject is shutting down");
                    return;
                }
                result = barrier_complete_rx.recv() =>{
                    let command_ctx_queue = &concurrent_control.command_ctx_queue;
                        let in_flight_nums = command_ctx_queue
                        .iter()
                        .filter(|x| matches!(x.states, InFlight))
                        .count();
                        self.metrics.in_flight_barrier_nums.set(in_flight_nums as i64);
                        self.metrics.all_barrier_nums.set(command_ctx_queue.len() as i64);

                    if let Some((prev_epoch,result)) = result{
                        self.barrier_complete_and_commit(prev_epoch,result,&mut state,&mut tracker,&mut concurrent_control).await;
                    }
                    continue;
                }
                // there's barrier scheduled.
                _ = self.scheduled_barriers.wait_one() ,if concurrent_control.can_inject_barrier(self.in_flight_barrier_nums) => {

                }
                // Wait for the minimal interval,
                _ = min_interval.tick() ,if concurrent_control.can_inject_barrier(self.in_flight_barrier_nums) => {

                }
            }

            if let Some(barrier_timer) = barrier_timer {
                barrier_timer.observe_duration();
            }
            barrier_timer = Some(self.metrics.barrier_send_latency.start_timer());

            let (command, notifiers) = self.scheduled_barriers.pop_or_default().await;
            if !matches!(command, Command::Plain(_)) {
                concurrent_control.is_build_actor = true;
            }
            let info = self.resolve_actor_info(command.creating_table_id()).await;
            // When there's no actors exist in the cluster, we don't need to send the barrier. This
            // is an advance optimization. Besides if another barrier comes immediately,
            // it may send a same epoch and fail the epoch check.
            if info.nothing_to_do() {
                let mut notifiers = notifiers;
                notifiers.iter_mut().for_each(Notifier::notify_to_send);
                notifiers.iter_mut().for_each(Notifier::notify_collected);
                continue;
            }
            let prev_epoch = state.in_flight_prev_epoch;
            let new_epoch = prev_epoch.next();
            state.in_flight_prev_epoch = new_epoch;
            assert!(
                new_epoch > prev_epoch,
                "new{:?},prev{:?}",
                new_epoch,
                prev_epoch
            );
            state
                .update_inflight_prev_epoch(self.env.meta_store())
                .await
                .unwrap();

            let command_ctx = Arc::new(CommandContext::new(
                self.fragment_manager.clone(),
                self.env.stream_client_pool_ref(),
                info,
                prev_epoch,
                new_epoch,
                command,
            ));
            let mut notifiers = notifiers;
            notifiers.iter_mut().for_each(Notifier::notify_to_send);
            let timer = self.metrics.barrier_latency.start_timer();
            concurrent_control.command_ctx_queue.push_back(EpochNode {
                timer: Some(timer),
                result: None,
                states: InFlight,
                command_ctx: command_ctx.clone(),
                notifiers,
            });
            let command_ctx = command_ctx.clone();
            self.inject_and_send_err(command_ctx, barrier_complete_tx.clone())
                .await;
        }
    }

    /// Inject barrier and send err.
    async fn inject_and_send_err(
        &self,
        command_context: Arc<CommandContext<S>>,
        barrier_complete_tx: UnboundedSender<(u64, Result<Vec<BarrierCompleteResponse>>)>,
    ) {
        let command_context_clone = command_context.clone();
        let result = self
            .inject_barrier(command_context_clone, barrier_complete_tx.clone())
            .await;
        if let Err(e) = result {
            tokio::spawn(async move {
                barrier_complete_tx
                    .send((command_context.prev_epoch.0, Err(e)))
                    .unwrap();
            });
        }
    }

    /// Send inject-barrier-rpc to stream service and wait for its response before returns.
    /// Then spawn a new tokio task to send barrier-complete-rpc and wait for its response
    async fn inject_barrier(
        &self,
        command_context: Arc<CommandContext<S>>,
        barrier_complete_tx: UnboundedSender<(u64, Result<Vec<BarrierCompleteResponse>>)>,
    ) -> Result<()> {
        fail_point!("inject_barrier_err", |_| Err(RwError::from(
            ErrorCode::InternalError("inject_barrier_err".to_string(),)
        )));
        let mutation = command_context.to_mutation().await?;
        let info = command_context.info.clone();
        let inject_futures = info.node_map.iter().filter_map(|(node_id, node)| {
            let actor_ids_to_send = info.actor_ids_to_send(node_id).collect_vec();
            let actor_ids_to_collect = info.actor_ids_to_collect(node_id).collect_vec();
            if actor_ids_to_collect.is_empty() {
                // No need to send or collect barrier for this node.
                assert!(actor_ids_to_send.is_empty());
                None
            } else {
                let mutation = mutation.clone();
                let request_id = Uuid::new_v4().to_string();
                let barrier = Barrier {
                    epoch: Some(risingwave_pb::data::Epoch {
                        curr: command_context.curr_epoch.0,
                        prev: command_context.prev_epoch.0,
                    }),
                    mutation,
                    // TODO(chi): add distributed tracing
                    span: vec![],
                };
                async move {
                    let mut client = self.env.stream_client_pool().get(node).await?;

                    let request = InjectBarrierRequest {
                        request_id,
                        barrier: Some(barrier),
                        actor_ids_to_send,
                        actor_ids_to_collect,
                    };
                    tracing::trace!(
                        target: "events::meta::barrier::inject_barrier",
                        "inject barrier request: {:?}", request
                    );

                    // This RPC returns only if this worker node has injected this barrier.
                    client
                        .inject_barrier(request)
                        .await
                        .map(tonic::Response::<_>::into_inner)
                        .map_err(RwError::from)
                }
                .into()
            }
        });
        try_join_all(inject_futures).await?;
        let env = self.env.clone();
        tokio::spawn(async move {
            let prev_epoch = command_context.prev_epoch.0;
            let collect_futures = info.node_map.iter().filter_map(|(node_id, node)| {
                let actor_ids_to_send = info.actor_ids_to_send(node_id).collect_vec();
                let actor_ids_to_collect = info.actor_ids_to_collect(node_id).collect_vec();
                if actor_ids_to_collect.is_empty() {
                    // No need to send or collect barrier for this node.
                    assert!(actor_ids_to_send.is_empty());
                    None
                } else {
                    let request_id = Uuid::new_v4().to_string();
                    let env = env.clone();
                    async move {
                        let mut client = env.stream_client_pool().get(node).await?;
                        let request = BarrierCompleteRequest {
                            request_id,
                            prev_epoch,
                        };
                        tracing::trace!(
                            target: "events::meta::barrier::barrier_complete",
                            "barrier complete request: {:?}", request
                        );

                        // This RPC returns only if this worker node has collected this barrier.
                        client
                            .barrier_complete(request)
                            .await
                            .map(tonic::Response::<_>::into_inner)
                            .map_err(RwError::from)
                    }
                    .into()
                }
            });

            let result = try_join_all(collect_futures).await;
            barrier_complete_tx.send((prev_epoch, result)).unwrap();
        });
        Ok(())
    }

    /// Changes the states is `Complete`, and try commit all epoch that states is `Complete` in
    /// order. If commit is err, changes the states is `Fail`. Then recovery or panic
    async fn barrier_complete_and_commit(
        &self,
        prev_epoch: u64,
        result: Result<Vec<BarrierCompleteResponse>>,
        state: &mut BarrierManagerState,
        tracker: &mut CreateMviewProgressTracker,
        concurrent_control: &mut ConcurrentControl<S>,
    ) {
        // change the states is Complete
        if let Some(node) = concurrent_control
            .command_ctx_queue
            .iter_mut()
            .find(|x| x.command_ctx.prev_epoch.0 == prev_epoch)
        {
            assert!(matches!(node.states, InFlight));
            node.states = Complete;
            node.result = Some(result);
        };
        if matches!(
            concurrent_control.command_ctx_queue.front().unwrap().states,
            Complete
        ) {
            // If the state of the first node is Complete, try commit all epoch
            let result = self.try_commit_epoch(concurrent_control, tracker).await;
            if result.is_err() {
                concurrent_control.is_recovery = true;
                concurrent_control
                    .command_ctx_queue
                    .front_mut()
                    .unwrap()
                    .states = Fail(result.unwrap_err());
            }
        }
        if !concurrent_control.command_ctx_queue.is_empty()
            && matches!(
                concurrent_control.command_ctx_queue.front().unwrap().states,
                Fail(_)
            )
            && concurrent_control
                .command_ctx_queue
                .iter()
                .filter(|x| matches!(x.states, InFlight))
                .count()
                == 0
        {
            // If the state of the first node is Fail, wait all barrier complete before recovery
            fail_point!("inject_barrier_err_success");
            let new_epoch = concurrent_control
                .command_ctx_queue
                .back()
                .unwrap()
                .command_ctx
                .curr_epoch;

            let err_msg = concurrent_control
                .command_ctx_queue
                .front()
                .unwrap()
                .result
                .clone()
                .unwrap()
                .err()
                .unwrap();
            while let Some(node) = concurrent_control.command_ctx_queue.pop_front() {
                let err = match node.states {
                    Fail(err) => err,
                    Complete => RwError::from(ErrorCode::InternalError(
                        "err from before epoch".to_string(),
                    )),
                    _ => {
                        panic!(
                            "fail recovery with pending : epoch{:?}",
                            node.command_ctx.prev_epoch
                        );
                    }
                };
                node.timer.unwrap().observe_duration();
                node.notifiers
                    .into_iter()
                    .for_each(|notifier| notifier.notify_collection_failed(err.clone()));
                if !matches!(node.command_ctx.command, Command::Plain(_)) {
                    concurrent_control.is_build_actor = false;
                }
            }

            if self.enable_recovery {
                // If failed, enter recovery mode.
                let (new_epoch, actors_to_track, create_mview_progress) =
                    self.recovery(new_epoch).await;
                *tracker = CreateMviewProgressTracker::default();
                tracker.add(new_epoch, actors_to_track, vec![]);
                for progress in create_mview_progress {
                    tracker.update(progress);
                }
                state.in_flight_prev_epoch = new_epoch;
                state
                    .update_inflight_prev_epoch(self.env.meta_store())
                    .await
                    .unwrap();
                concurrent_control.is_recovery = false;
            } else {
                panic!("failed to execute barrier: {:?}", err_msg);
            }
        }
    }

    /// Try to commit all `Complete` from head to `InFlight` and pop them. If err, this commit will
    /// be stop and return.
    async fn try_commit_epoch(
        &self,
        concurrent_control: &mut ConcurrentControl<S>,
        tracker: &mut CreateMviewProgressTracker,
    ) -> Result<()> {
        while let Some(node) = concurrent_control.command_ctx_queue.front_mut() {
            if !matches!(node.states, Complete) {
                break;
            }
            if node.command_ctx.prev_epoch.0 != INVALID_EPOCH {
                match &node
                    .result
                    .as_mut()
                    .unwrap_or_else(|| panic!("node result is none"))
                {
                    Ok(resps) => {
                        // We must ensure all epochs are committed in ascending order,
                        // because the storage engine will
                        // query from new to old in the order in which the L0 layer files are generated. see https://github.com/singularity-data/risingwave/issues/1251
                        let synced_ssts: Vec<LocalSstableInfo> = resps
                            .iter()
                            .flat_map(|resp| resp.sycned_sstables.clone())
                            .map(|grouped| {
                                (
                                    grouped.compaction_group_id,
                                    grouped.sst.expect("field not None"),
                                )
                            })
                            .collect_vec();
                        self.hummock_manager
                            .commit_epoch(node.command_ctx.prev_epoch.0, synced_ssts)
                            .await?;
                    }
                    Err(err) => {
                        tracing::warn!(
                            "Failed to commit epoch {}: {:#?}",
                            node.command_ctx.prev_epoch.0,
                            err
                        );
                        return Err(err.clone());
                    }
                };
            }

            node.timer.take().unwrap().observe_duration();
            node.command_ctx.post_collect().await?;

            // this barrier is commit (not err) , So this node need to pop;
            let node = concurrent_control.command_ctx_queue.pop_front().unwrap();
            let EpochNode {
                result,
                command_ctx,
                mut notifiers,
                ..
            } = node;
            let responses = result.unwrap().unwrap();
            // Notify about collected first.
            notifiers.iter_mut().for_each(Notifier::notify_collected);
            // Then try to finish the barrier for Create MVs.
            let actors_to_finish = command_ctx.actors_to_track();
            tracker.add(command_ctx.curr_epoch, actors_to_finish, notifiers);
            for progress in responses.into_iter().flat_map(|r| r.create_mview_progress) {
                tracker.update(progress);
            }
            if !matches!(command_ctx.command, Command::Plain(_)) {
                concurrent_control.is_build_actor = false;
            }
        }
        Ok(())
    }

    /// Resolve actor information from cluster and fragment manager.
    async fn resolve_actor_info(&self, creating_table_id: Option<TableId>) -> BarrierActorInfo {
        let all_nodes = self
            .cluster_manager
            .list_worker_node(WorkerType::ComputeNode, Some(Running))
            .await;
        let all_actor_infos = self
            .fragment_manager
            .load_all_actors(creating_table_id)
            .await;
        BarrierActorInfo::resolve(all_nodes, all_actor_infos)
    }

    async fn do_schedule(&self, command: Command, notifier: Notifier) -> Result<()> {
        self.scheduled_barriers
            .push((command, once(notifier).collect()))
            .await;
        Ok(())
    }

    /// Schedule a command and return immediately.
    pub async fn schedule_command(&self, command: Command) -> Result<()> {
        self.do_schedule(command, Default::default()).await
    }

    /// Schedule a command and return when its corresponding barrier is about to sent.
    pub async fn issue_command(&self, command: Command) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.do_schedule(
            command,
            Notifier {
                to_send: Some(tx),
                ..Default::default()
            },
        )
        .await?;
        rx.await.unwrap();

        Ok(())
    }

    /// Run a command and return when it's completely finished.
    pub async fn run_command(&self, command: Command) -> Result<()> {
        let (collect_tx, collect_rx) = oneshot::channel();
        let (finish_tx, finish_rx) = oneshot::channel();

        let is_create_mv = matches!(command, Command::CreateMaterializedView { .. });

        self.do_schedule(
            command,
            Notifier {
                collected: Some(collect_tx),
                finished: Some(finish_tx),
                ..Default::default()
            },
        )
        .await?;

        collect_rx.await.unwrap()?; // Throw the error if it occurs when collecting this barrier.

        // TODO: refactor this
        if is_create_mv {
            // The snapshot ingestion may last for several epochs, we should pin the epoch here.
            // TODO: this should be done in `post_collect`
            let snapshot = self
                .hummock_manager
                .pin_snapshot(META_NODE_ID, HummockEpoch::MAX)
                .await?;
            finish_rx.await.unwrap(); // Wait for this command to be finished.
            self.hummock_manager
                .unpin_snapshot(META_NODE_ID, [snapshot])
                .await?;
        } else {
            finish_rx.await.unwrap(); // Wait for this command to be finished.
        }

        Ok(())
    }

    /// Wait for the next barrier to collect. Note that the barrier flowing in our stream graph is
    /// ignored, if exists.
    pub async fn wait_for_next_barrier_to_collect(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        let notifier = Notifier {
            collected: Some(tx),
            ..Default::default()
        };
        self.scheduled_barriers
            .attach_notifiers(once(notifier))
            .await;
        rx.await.unwrap()
    }
}

pub type BarrierManagerRef<S> = Arc<GlobalBarrierManager<S>>;
