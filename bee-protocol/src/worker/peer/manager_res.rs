// Copyright 2020 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

// TODO This exist to avoid a cyclic dependency, there has to be another way.

use crate::peer::PeerManager;

use bee_runtime::{node::Node, worker::Worker};

use async_trait::async_trait;
use log::warn;

use std::convert::Infallible;

pub(crate) struct PeerManagerResWorker {}

#[async_trait]
impl<N: Node> Worker<N> for PeerManagerResWorker {
    type Config = ();
    type Error = Infallible;

    async fn start(node: &mut N, _config: Self::Config) -> Result<Self, Self::Error> {
        node.register_resource(PeerManager::new());

        Ok(Self {})
    }

    async fn stop(self, node: &mut N) -> Result<(), Self::Error> {
        let peer_manager = node.resource::<PeerManager>();

        for (_, (peer, ctx)) in peer_manager.peers.write().await.iter_mut() {
            if let Some((_, shutdown, fut)) = ctx.take() {
                if let Err(e) = shutdown.send(()) {
                    warn!("Sending shutdown to {} failed: {:?}.", peer.alias(), e);
                }
                let _ = fut.await;
            }
        }

        node.remove_resource::<PeerManager>();

        Ok(())
    }
}
