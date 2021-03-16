// Copyright 2020 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

pub mod api;
pub mod debug;
pub mod health;

use crate::endpoints::{config::RestApiConfig, storage::StorageBackend, Bech32Hrp, NetworkId};

use bee_network::NetworkServiceController;
use bee_protocol::{config::ProtocolConfig, MessageSubmitterWorkerEvent, PeerManager};
use bee_runtime::{node::NodeInfo, resource::ResourceHandle};
use bee_tangle::MsTangle;

use tokio::sync::mpsc;
use warp::{self, Filter, Rejection, Reply};

use std::net::IpAddr;

pub(crate) fn filter_all<B: StorageBackend>(
    public_routes: Vec<String>,
    allowed_ips: Vec<IpAddr>,
    tangle: ResourceHandle<MsTangle<B>>,
    storage: ResourceHandle<B>,
    message_submitter: mpsc::UnboundedSender<MessageSubmitterWorkerEvent>,
    network_id: NetworkId,
    bech32_hrp: Bech32Hrp,
    rest_api_config: RestApiConfig,
    protocol_config: ProtocolConfig,
    peer_manager: ResourceHandle<PeerManager>,
    network_controller: ResourceHandle<NetworkServiceController>,
    node_info: ResourceHandle<NodeInfo>,
) -> impl Filter<Extract = impl Reply, Error = Rejection> + Clone {
    api::filter(
        public_routes.clone(),
        allowed_ips.clone(),
        tangle.clone(),
        storage,
        message_submitter,
        network_id,
        bech32_hrp,
        rest_api_config,
        protocol_config,
        peer_manager.clone(),
        network_controller,
        node_info,
    )
    .or(debug::filter(public_routes.clone(), allowed_ips.clone()))
    .or(health::filter(public_routes, allowed_ips, tangle, peer_manager))
}
