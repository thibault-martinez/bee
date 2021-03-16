// Copyright 2020 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use crate::{
    endpoints::{
        body::{BodyInner, SuccessBody},
        config::ROUTE_RECEIPTS,
        filters::with_storage,
        permission::has_permission,
        rejection::CustomRejection,
        storage::StorageBackend,
    },
    types::ReceiptDto,
};

use bee_ledger::types::Receipt;
use bee_message::milestone::MilestoneIndex;
use bee_runtime::resource::ResourceHandle;
use bee_storage::access::AsStream;

use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use warp::{Filter, Rejection, Reply};

use std::{convert::TryFrom, net::IpAddr};

/// Response of GET /api/v1/receipts/{milestone_index} and /api/v1/receipts
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReceiptsResponse(pub Vec<ReceiptDto>);

impl BodyInner for ReceiptsResponse {}

fn path() -> impl Filter<Extract = (), Error = Rejection> + Clone {
    super::path().and(warp::path("receipts")).and(warp::path::end())
}

pub(crate) fn filter<B: StorageBackend>(
    public_routes: Vec<String>,
    allowed_ips: Vec<IpAddr>,
    storage: ResourceHandle<B>,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    self::path()
        .and(warp::get())
        .and(has_permission(ROUTE_RECEIPTS, public_routes, allowed_ips))
        .and(with_storage(storage))
        .and_then(receipts)
}

pub(crate) async fn receipts<B: StorageBackend>(storage: ResourceHandle<B>) -> Result<impl Reply, Rejection> {
    let mut receipts_dto = Vec::new();
    let mut stream = AsStream::<(MilestoneIndex, Receipt), ()>::stream(&*storage)
        .await
        .map_err(|_| CustomRejection::InternalError)?;

    while let Some(((_, receipt), _)) = stream.next().await {
        receipts_dto.push(ReceiptDto::try_from(receipt).map_err(|_| CustomRejection::InternalError)?);
    }

    Ok(warp::reply::json(&SuccessBody::new(ReceiptsResponse(receipts_dto))))
}
