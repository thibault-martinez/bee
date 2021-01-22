// Copyright 2020 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use crate::{
    event::LatestSolidMilestoneChanged,
    helper,
    peer::PeerManager,
    storage::StorageBackend,
    worker::{
        MessageRequesterWorker, MessageRequesterWorkerEvent, MetricsWorker, MilestoneConeUpdaterWorker,
        MilestoneConeUpdaterWorkerEvent, MilestoneRequesterWorker, PeerManagerResWorker, RequestedMessages,
        RequestedMilestones, TangleWorker,
    },
    ProtocolMetrics,
};

use bee_ledger::{LedgerWorker, LedgerWorkerEvent};
use bee_message::{
    milestone::{Milestone, MilestoneIndex},
    MessageId,
};
use bee_runtime::{event::Bus, node::Node, shutdown_stream::ShutdownStream, worker::Worker};
use bee_tangle::{traversal, MsTangle};

use async_trait::async_trait;
use futures::StreamExt;
use log::{debug, info, warn};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use std::{any::TypeId, cmp, collections::HashSet, convert::Infallible};

pub(crate) struct MilestoneSolidifierWorkerEvent(pub MilestoneIndex);

pub(crate) struct MilestoneSolidifierWorker {
    pub(crate) tx: mpsc::UnboundedSender<MilestoneSolidifierWorkerEvent>,
}

async fn heavy_solidification<B: StorageBackend>(
    tangle: &MsTangle<B>,
    message_requester: &mpsc::UnboundedSender<MessageRequesterWorkerEvent>,
    requested_messages: &RequestedMessages,
    target_index: MilestoneIndex,
    target_id: MessageId,
) -> (bool, HashSet<MessageId>) {
    // TODO: This wouldn't be necessary if the traversal code wasn't closure-driven
    let mut missing = HashSet::new();
    let mut visited = HashSet::new();

    traversal::visit_parents_depth_first(
        &**tangle,
        target_id,
        |id, _, metadata| async move {
            (!metadata.flags().is_requested() || id == target_id)
                && !metadata.flags().is_solid()
                && !requested_messages.contains(&id).await
        },
        |visited_id, _, _| {
            visited.insert(*visited_id);
        },
        |_, _, _| {},
        |missing_id| {
            missing.insert(*missing_id);
        },
    )
    .await;

    debug!(
        "Heavy solidification of milestone {} {}: {} messages requested.",
        *target_index,
        target_id,
        missing.len()
    );

    let solid = missing.is_empty();

    for missing_id in missing {
        helper::request_message(tangle, message_requester, requested_messages, missing_id, target_index).await;
    }

    (solid, visited)
}

async fn light_solidification<B: StorageBackend>(
    tangle: &MsTangle<B>,
    message_requester: &mpsc::UnboundedSender<MessageRequesterWorkerEvent>,
    requested_messages: &RequestedMessages,
    target_index: MilestoneIndex,
    target_id: MessageId,
    parent1: MessageId,
    parent2: MessageId,
) {
    debug!("Light solidification of milestone {} {}.", *target_index, target_id);

    helper::request_message(tangle, message_requester, requested_messages, parent1, target_index).await;
    if parent1 != parent2 {
        helper::request_message(tangle, message_requester, requested_messages, parent2, target_index).await;
    }
}

async fn solidify<B: StorageBackend>(
    tangle: &MsTangle<B>,
    ledger_worker: &mpsc::UnboundedSender<LedgerWorkerEvent>,
    milestone_cone_updater: &mpsc::UnboundedSender<MilestoneConeUpdaterWorkerEvent>,
    peer_manager: &PeerManager,
    metrics: &ProtocolMetrics,
    bus: &Bus<'static>,
    id: MessageId,
    index: MilestoneIndex,
    visisted: HashSet<MessageId>,
) {
    debug!("New solid milestone {}.", *index);

    for v in visisted {
        tangle
            .update_metadata(&v, |metadata| {
                metadata.solidify();
            })
            .await;
    }

    tangle.update_latest_solid_milestone_index(index);

    if let Err(e) = ledger_worker.send(LedgerWorkerEvent(id)) {
        warn!("Sending message_id to ledger worker failed: {}.", e);
    }

    if let Err(e) = milestone_cone_updater
        // TODO get MS
        .send(MilestoneConeUpdaterWorkerEvent(index, Milestone::new(id, 0)))
    {
        warn!("Sending message_id to `MilestoneConeUpdater` failed: {:?}.", e);
    }

    helper::broadcast_heartbeat(
        &peer_manager,
        &metrics,
        index,
        tangle.get_pruning_index(),
        tangle.get_latest_milestone_index(),
    )
    .await;

    bus.dispatch(LatestSolidMilestoneChanged {
        index,
        // TODO get MS
        milestone: Milestone::new(id, 0),
    });
}

#[async_trait]
impl<N: Node> Worker<N> for MilestoneSolidifierWorker
where
    N::Backend: StorageBackend,
{
    type Config = u32;
    type Error = Infallible;

    fn dependencies() -> &'static [TypeId] {
        vec![
            TypeId::of::<MessageRequesterWorker>(),
            TypeId::of::<MilestoneRequesterWorker>(),
            TypeId::of::<TangleWorker>(),
            TypeId::of::<PeerManagerResWorker>(),
            TypeId::of::<MetricsWorker>(),
            TypeId::of::<LedgerWorker>(),
            TypeId::of::<MilestoneConeUpdaterWorker>(),
        ]
        .leak()
    }

    async fn start(node: &mut N, config: Self::Config) -> Result<Self, Self::Error> {
        let (tx, rx) = mpsc::unbounded_channel();
        let message_requester = node.worker::<MessageRequesterWorker>().unwrap().tx.clone();
        let milestone_requester = node.worker::<MilestoneRequesterWorker>().unwrap().tx.clone();
        let ledger_worker = node.worker::<LedgerWorker>().unwrap().tx.clone();
        let milestone_cone_updater = node.worker::<MilestoneConeUpdaterWorker>().unwrap().tx.clone();
        let tangle = node.resource::<MsTangle<N::Backend>>();
        let requested_messages = node.resource::<RequestedMessages>();
        let requested_milestones = node.resource::<RequestedMilestones>();
        let metrics = node.resource::<ProtocolMetrics>();
        let peer_manager = node.resource::<PeerManager>();
        let bus = node.bus();
        let ms_sync_count = config;

        node.spawn::<Self, _, _>(|shutdown| async move {
            info!("Running.");

            let mut receiver = ShutdownStream::new(shutdown, UnboundedReceiverStream::new(rx));

            let mut requested = tangle.get_latest_solid_milestone_index() + MilestoneIndex(1);

            while let Some(MilestoneSolidifierWorkerEvent(index)) = receiver.next().await {
                let lsmi = tangle.get_latest_solid_milestone_index();
                let lmi = tangle.get_latest_milestone_index();

                // Request milestones in a range.
                while requested <= cmp::min(lsmi + MilestoneIndex(ms_sync_count), lmi) {
                    helper::request_milestone(&tangle, &milestone_requester, &*requested_milestones, requested, None)
                        .await;
                    requested = requested + MilestoneIndex(1);
                }

                if index < requested {
                    if let Some(id) = tangle.get_milestone_message_id(index).await {
                        if let Some(message) = tangle.get(&id).await {
                            if tangle.contains(message.parent1()).await || tangle.contains(message.parent2()).await {
                            } else {
                                light_solidification(
                                    &tangle,
                                    &message_requester,
                                    &requested_messages,
                                    index,
                                    id,
                                    *message.parent1(),
                                    *message.parent2(),
                                )
                                .await;
                            }
                        }
                    }
                }

                let mut target = lsmi + MilestoneIndex(1);

                while target <= cmp::min(lsmi + MilestoneIndex(ms_sync_count), lmi) {
                    if let Some(id) = tangle.get_milestone_message_id(target).await {
                        let (solid, visisted) =
                            heavy_solidification(&tangle, &message_requester, &requested_messages, target, id).await;

                        if solid {
                            solidify(
                                &tangle,
                                &ledger_worker,
                                &milestone_cone_updater,
                                &peer_manager,
                                &metrics,
                                &bus,
                                id,
                                index,
                                visisted,
                            )
                            .await;
                        } else {
                            break;
                        }
                    }
                    target = target + MilestoneIndex(1);
                }
            }

            info!("Stopped.");
        });

        // TODO handle
        let _ = tx.send(MilestoneSolidifierWorkerEvent(MilestoneIndex(0)));

        Ok(Self { tx })
    }
}
