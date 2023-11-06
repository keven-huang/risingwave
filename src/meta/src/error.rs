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

use std::backtrace::Backtrace;
use std::sync::Arc;

use aws_sdk_ec2::error::DisplayErrorContext;
use risingwave_common::error::BoxedError;
use risingwave_connector::sink::SinkError;
use risingwave_pb::PbFieldNotFound;
use risingwave_rpc_client::error::RpcError;

use crate::hummock::error::Error as HummockError;
use crate::manager::WorkerId;
use crate::model::MetadataModelError;
use crate::storage::MetaStoreError;

pub type MetaResult<T> = std::result::Result<T, MetaError>;

#[derive(thiserror::Error, Debug)]
enum MetaErrorInner {
    #[error("MetaStore transaction error: {0}")]
    TransactionError(MetaStoreError),

    #[error("MetadataModel error: {0}")]
    MetadataModelError(MetadataModelError),

    #[error("Hummock error: {0}")]
    HummockError(HummockError),

    #[error("Rpc error: {0}")]
    RpcError(RpcError),

    #[error("PermissionDenied: {0}")]
    PermissionDenied(String),

    #[error("Invalid worker: {0}, {1}")]
    InvalidWorker(WorkerId, String),

    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),

    // Used for catalog errors.
    #[error("{0} id not found: {1}")]
    CatalogIdNotFound(&'static str, String),

    #[error("table_fragment not exist: id={0}")]
    FragmentNotFound(u32),

    #[error("{0} with name {1} exists")]
    Duplicated(&'static str, String),

    #[error("Service unavailable: {0}")]
    Unavailable(String),

    #[error("Election failed: {0}")]
    Election(String),

    #[error("Cancelled: {0}")]
    Cancelled(String),

    #[error("SystemParams error: {0}")]
    SystemParams(String),

    #[error("Sink error: {0}")]
    Sink(SinkError),

    #[error("AWS SDK error: {}", DisplayErrorContext(& * *.0))]
    Aws(BoxedError),

    #[error(transparent)]
    Internal(anyhow::Error),
}

impl From<MetaErrorInner> for MetaError {
    fn from(inner: MetaErrorInner) -> Self {
        Self {
            inner: Arc::new(inner),
            backtrace: Arc::new(Backtrace::capture()),
        }
    }
}

#[derive(thiserror::Error, Clone)]
#[error("{inner}")]
pub struct MetaError {
    inner: Arc<MetaErrorInner>,
    backtrace: Arc<Backtrace>,
}

impl std::fmt::Debug for MetaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use std::error::Error;

        write!(f, "{}", self.inner)?;
        writeln!(f)?;
        if let Some(backtrace) = std::error::request_ref::<Backtrace>(&self.inner as &dyn Error) {
            write!(f, "  backtrace of inner error:\n{}", backtrace)?;
        } else {
            write!(f, "  backtrace of `MetaError`:\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl MetaError {
    /// Permission denied error.
    pub fn permission_denied(s: String) -> Self {
        MetaErrorInner::PermissionDenied(s).into()
    }

    pub fn invalid_worker(worker_id: WorkerId, msg: String) -> Self {
        MetaErrorInner::InvalidWorker(worker_id, msg).into()
    }

    pub fn is_invalid_worker(&self) -> bool {
        use std::borrow::Borrow;
        std::matches!(self.inner.borrow(), &MetaErrorInner::InvalidWorker(..))
    }

    pub fn invalid_parameter(s: impl Into<String>) -> Self {
        MetaErrorInner::InvalidParameter(s.into()).into()
    }

    pub fn catalog_id_not_found<T: ToString>(relation: &'static str, id: T) -> Self {
        MetaErrorInner::CatalogIdNotFound(relation, id.to_string()).into()
    }

    pub fn fragment_not_found<T: Into<u32>>(id: T) -> Self {
        MetaErrorInner::FragmentNotFound(id.into()).into()
    }

    pub fn is_fragment_not_found(&self) -> bool {
        matches!(self.inner.as_ref(), &MetaErrorInner::FragmentNotFound(..))
    }

    pub fn catalog_duplicated<T: Into<String>>(relation: &'static str, name: T) -> Self {
        MetaErrorInner::Duplicated(relation, name.into()).into()
    }

    pub fn system_param<T: ToString>(s: T) -> Self {
        MetaErrorInner::SystemParams(s.to_string()).into()
    }

    pub fn unavailable(s: String) -> Self {
        MetaErrorInner::Unavailable(s).into()
    }

    pub fn cancelled(s: String) -> Self {
        MetaErrorInner::Cancelled(s).into()
    }
}

impl From<MetadataModelError> for MetaError {
    fn from(e: MetadataModelError) -> Self {
        MetaErrorInner::MetadataModelError(e).into()
    }
}

impl From<HummockError> for MetaError {
    fn from(e: HummockError) -> Self {
        MetaErrorInner::HummockError(e).into()
    }
}

impl From<etcd_client::Error> for MetaError {
    fn from(e: etcd_client::Error) -> Self {
        MetaErrorInner::Election(e.to_string()).into()
    }
}

impl From<RpcError> for MetaError {
    fn from(e: RpcError) -> Self {
        MetaErrorInner::RpcError(e).into()
    }
}

impl From<SinkError> for MetaError {
    fn from(e: SinkError) -> Self {
        MetaErrorInner::Sink(e).into()
    }
}

impl From<anyhow::Error> for MetaError {
    fn from(a: anyhow::Error) -> Self {
        MetaErrorInner::Internal(a).into()
    }
}

impl<E> From<aws_sdk_ec2::error::SdkError<E>> for MetaError
where
    E: std::error::Error + Sync + Send + 'static,
{
    fn from(e: aws_sdk_ec2::error::SdkError<E>) -> Self {
        MetaErrorInner::Aws(e.into()).into()
    }
}

impl From<MetaError> for tonic::Status {
    fn from(err: MetaError) -> Self {
        match &*err.inner {
            MetaErrorInner::PermissionDenied(_) => {
                tonic::Status::permission_denied(err.to_string())
            }
            MetaErrorInner::CatalogIdNotFound(_, _) => tonic::Status::not_found(err.to_string()),
            MetaErrorInner::Duplicated(_, _) => tonic::Status::already_exists(err.to_string()),
            MetaErrorInner::Unavailable(_) => tonic::Status::unavailable(err.to_string()),
            MetaErrorInner::Cancelled(_) => tonic::Status::cancelled(err.to_string()),
            MetaErrorInner::InvalidParameter(msg) => {
                tonic::Status::invalid_argument(msg.to_owned())
            }
            _ => tonic::Status::internal(err.to_string()),
        }
    }
}

impl From<PbFieldNotFound> for MetaError {
    fn from(e: PbFieldNotFound) -> Self {
        MetadataModelError::from(e).into()
    }
}

impl From<MetaStoreError> for MetaError {
    fn from(e: MetaStoreError) -> Self {
        match e {
            // `MetaStore::txn` method error.
            MetaStoreError::TransactionAbort() => MetaErrorInner::TransactionError(e).into(),
            _ => MetadataModelError::from(e).into(),
        }
    }
}
